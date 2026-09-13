//! The agent loop (task M01-08) against `AppState` with the mentor
//! pointed at a wiremock server that plays a script of recorded SSE
//! responses: the three-step trajectory and its trace, denied tools,
//! cancellation during tools and during streaming (and the run after),
//! the `max_tokens` continuation, the iteration and context guards, a
//! refusal, a rate limit, a broken stream, the permission prompt through
//! the handle, and the stability of the tool set across steps.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apprentice_api::events::{AgentStatus, Event, StepPhase};
use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::SessionCreateParams;
use apprentice_api::types::{
    PermissionAnswer, PermissionDecision, PermissionMode, PermissionSource, RunOptions, TraceEvent,
};
use apprentice_core::app::AppState;
use apprentice_core::config::{ConfigLoader, Paths};
use apprentice_core::mentor::MentorRequest;
use apprentice_core::runtime::{
    AgentHandle, CONTINUE_MESSAGE, Conversation, MENTOR_SYSTEM_V1, PROMPT_VERSION, run_agent,
};
use apprentice_core::secrets::{Secret, SecretStore as _, api_key_name};
use apprentice_core::tools::{Risk, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};
use apprentice_core::trace::{BlobId, RunStatus, SessionId, TraceStore, kinds};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn fixture(name: &str) -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sse")
        .join(format!("{name}.txt"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn sse(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(fixture(name), "text/event-stream")
}

/// Plays responses in order; a request past the end gets a 500 that
/// names the overrun.
struct Script(Mutex<VecDeque<ResponseTemplate>>);

impl Respond for Script {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        self.0.lock().unwrap().pop_front().unwrap_or_else(|| {
            ResponseTemplate::new(500).set_body_json(json!({
                "type": "error",
                "error": {"type": "api_error", "message": "script exhausted"}
            }))
        })
    }
}

/// A tool that runs until the agent is cancelled.
struct Slow;

#[async_trait]
impl Tool for Slow {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "slow",
            "Waits.",
            json!({"type": "object", "properties": {}}),
            Risk::ReadOnly,
        )
    }

    async fn call(
        &self,
        _: &ToolContext,
        _: Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        cancel.cancelled().await;
        Err(ToolError::Cancelled)
    }
}

struct Harness {
    _home: tempfile::TempDir,
    workspace: tempfile::TempDir,
    state: Arc<AppState>,
    server: MockServer,
}

impl Harness {
    /// A state whose mentor talks to a wiremock server, a key in the
    /// file secret store, and a workspace with `src/main.rs` and
    /// `Cargo.toml` (what the recorded reads ask for). `extra` is
    /// appended to the user config.
    async fn new(extra: &str) -> Self {
        let server = MockServer::start().await;
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(home.path());
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        std::fs::write(
            paths.config_file(),
            format!(
                "[daemon]\nsecret_store = \"file\"\n\n[sessions]\nauto_title = false\n\n[mentor]\nbase_url = \"{}\"\nmax_retries = 0\ntimeout_s = 30\n{extra}",
                server.uri()
            ),
        )
        .unwrap();
        let loader = ConfigLoader::new(paths);
        let config = loader.load(None).unwrap().config;
        let state = AppState::open_with(loader, &config).unwrap();
        state
            .secrets()
            .set(&api_key_name("anthropic"), &Secret::new("sk-ant-test"))
            .unwrap();
        state.tools().register(Arc::new(Slow)).unwrap();
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("src")).unwrap();
        std::fs::write(
            workspace.path().join("src/main.rs"),
            "fn main() {\n    println!(\"hi\");\n}\n",
        )
        .unwrap();
        std::fs::write(
            workspace.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\n",
        )
        .unwrap();
        Self {
            _home: home,
            workspace,
            state,
            server,
        }
    }

    async fn script(&self, responses: Vec<ResponseTemplate>) {
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(Script(Mutex::new(responses.into())))
            .mount(&self.server)
            .await;
    }

    async fn session(&self) -> SessionId {
        self.state
            .session_create(&SessionCreateParams {
                workspace: Some(self.workspace.path().to_string_lossy().into_owned()),
                title: Some("loop test".into()),
            })
            .await
            .unwrap()
            .session_id
            .into()
    }

    fn store(&self) -> &TraceStore {
        self.state.store()
    }

    async fn run(
        &self,
        session: &SessionId,
        prompt: &str,
        mode: PermissionMode,
    ) -> (AgentHandle, broadcast::Receiver<Event>) {
        run_agent(
            &self.state,
            session.clone(),
            prompt.into(),
            RunOptions {
                permission_mode: Some(mode),
                ..RunOptions::default()
            },
        )
        .await
        .unwrap()
    }

    /// The request bodies the server received, in order.
    async fn requests(&self) -> Vec<Value> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect()
    }
}

/// Collects events until the terminal one.
async fn collect(rx: &mut broadcast::Receiver<Event>) -> Vec<Event> {
    let mut out = Vec::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(15), rx.recv())
            .await
            .expect("agent finished in time")
            .expect("channel open");
        let done = ev.is_terminal();
        out.push(ev);
        if done {
            return out;
        }
    }
}

fn finished(events: &[Event]) -> (AgentStatus, Option<&RpcError>, bool) {
    match events.last() {
        Some(Event::AgentFinished {
            status,
            error,
            truncated,
            ..
        }) => (*status, error.as_ref(), *truncated),
        other => panic!("expected agent.finished, got {other:?}"),
    }
}

fn kinds_of(events: &[TraceEvent]) -> Vec<&str> {
    events.iter().map(|e| e.summary.kind.as_str()).collect()
}

fn of_kind<'a>(events: &'a [TraceEvent], kind: &str) -> Vec<&'a TraceEvent> {
    events.iter().filter(|e| e.summary.kind == kind).collect()
}

fn blob(store: &TraceStore, ev: &TraceEvent) -> Vec<u8> {
    let id = ev
        .blob_id
        .as_deref()
        .unwrap_or_else(|| panic!("{} has no blob", ev.summary.kind));
    store.read_blob(&BlobId::from(id)).unwrap()
}

/// The history a request body carries, checked against the API's
/// rules.
fn history(body: &Value) -> Conversation {
    let req: MentorRequest = serde_json::from_value(body.clone()).unwrap();
    let conv = Conversation::load(SessionId::from("check"), req.messages);
    conv.validate()
        .unwrap_or_else(|e| panic!("invalid history: {e}\n{body:#}"));
    conv
}

#[tokio::test]
async fn a_three_step_trajectory_streams_events_and_records_every_step() {
    let h = Harness::new("").await;
    h.script(vec![
        sse("tool_use_parallel"),
        sse("tool_use_write"),
        sse("text"),
    ])
    .await;
    let session = h.session().await;
    let (handle, mut rx) = h.run(&session, "add hello", PermissionMode::Auto).await;
    let events = collect(&mut rx).await;
    assert_eq!(finished(&events), (AgentStatus::Ok, None, false));

    // The event sequence, permission decisions aside (three, one per
    // call, all allowed without asking).
    let decisions: Vec<(PermissionDecision, PermissionSource)> = events
        .iter()
        .filter_map(|e| match e {
            Event::PermissionDecision {
                decision, source, ..
            } => Some((*decision, *source)),
            _ => None,
        })
        .collect();
    assert_eq!(
        decisions,
        [
            (PermissionDecision::Allow, PermissionSource::Rule),
            (PermissionDecision::Allow, PermissionSource::Rule),
            (PermissionDecision::Allow, PermissionSource::Mode),
        ]
    );
    let rest: Vec<&Event> = events
        .iter()
        .filter(|e| !matches!(e, Event::PermissionDecision { .. }))
        .collect();
    let mut seq: Vec<String> = rest
        .iter()
        .map(|e| match e {
            Event::AgentStep { seq, phase, .. } => format!("step {seq} {phase:?}"),
            Event::AgentToolCall { name, call_id, .. } => format!("call {name} {call_id}"),
            Event::AgentToolResult {
                name, ok, call_id, ..
            } => format!("result {name} {call_id} ok={ok}"),
            Event::AgentTextDelta { text, .. } => format!("text {text:?}"),
            Event::AgentUsage { .. } => "usage".into(),
            other => serde_json::to_value(other).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned(),
        })
        .collect();
    // The two reads run concurrently: their results come in any order.
    let first_result = seq.iter().position(|s| s.starts_with("result ")).unwrap();
    seq[first_result..first_result + 2].sort();
    assert_eq!(
        seq,
        [
            "agent.started",
            "step 1 Mentor",
            "text \"I'll read both files.\"",
            "call read_file toolu_01A",
            "call read_file toolu_01B",
            "usage",
            "step 1 Tools",
            "result read_file toolu_01A ok=true",
            "result read_file toolu_01B ok=true",
            "step 2 Mentor",
            "text \"Adding the file.\"",
            "call write_file toolu_01C",
            "usage",
            "step 2 Tools",
            "result write_file toolu_01C ok=true",
            "step 3 Mentor",
            "text \"Hello\"",
            "text \", \"",
            "text \"world\"",
            "text \"!\"",
            "usage",
            "agent.finished",
        ]
    );
    let usage: Vec<(u64, u64, u64)> = events
        .iter()
        .filter_map(|e| match e {
            Event::AgentUsage {
                usage,
                session_usage,
                session_calls,
                ..
            } => Some((
                usage.input_tokens,
                session_usage.input_tokens,
                *session_calls,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(usage, [(412, 412, 1), (650, 1062, 2), (25, 1087, 3)]);
    assert!(
        h.workspace.path().join("src/hello.rs").is_file(),
        "the write ran"
    );

    // The requests: tools identical every call, the results of a step
    // batched into one user message in order, breakpoints where SPEC
    // §6 puts them.
    let bodies = h.requests().await;
    assert_eq!(bodies.len(), 3);
    let tools: Vec<&Value> = bodies.iter().map(|b| &b["tools"]).collect();
    assert!(tools.iter().all(|t| *t == tools[0]), "tool set changed");
    let names: Vec<&str> = tools[0]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
    assert!(names.contains(&"slow") && names.contains(&"write_file"));
    assert!(bodies.iter().all(|b| b["system"] == bodies[0]["system"]));
    // The system prompt (M01-09): the frozen core, then the workspace
    // block, a breakpoint on each.
    let system = bodies[0]["system"].as_array().unwrap();
    assert_eq!(system.len(), 2);
    assert_eq!(system[0]["text"], MENTOR_SYSTEM_V1);
    let context = system[1]["text"].as_str().unwrap();
    assert!(context.starts_with("#workspace\nroot: "), "{context}");
    assert!(
        context.contains("\ngit: not a git repository\n"),
        "{context}"
    );
    assert!(
        context.contains("\nlanguages: rust 50%, toml 50%\n"),
        "{context}"
    );
    assert!(
        context.contains("\ntop-level: src/, Cargo.toml\n"),
        "{context}"
    );
    assert!(!context.contains("#instructions"), "{context}");
    assert!(
        system
            .iter()
            .all(|s| s["cache_control"]["type"] == "ephemeral")
    );
    let messages = bodies[2]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 5);
    let roles: Vec<&str> = messages
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["user", "assistant", "user", "assistant", "user"]);
    assert_eq!(messages[0]["content"][0]["text"], "add hello");
    assert!(messages[0]["content"][0].get("cache_control").is_none());
    let results = messages[2]["content"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["tool_use_id"], "toolu_01A");
    assert_eq!(results[1]["tool_use_id"], "toolu_01B");
    assert!(
        results[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("println!"),
        "{results:#?}"
    );
    assert_eq!(messages[1]["content"][1]["type"], "tool_use");
    assert_eq!(messages[3]["content"][1]["name"], "write_file");
    assert_eq!(messages[4]["content"][0]["tool_use_id"], "toolu_01C");
    assert_eq!(
        messages[4]["content"][0]["cache_control"]["type"], "ephemeral",
        "the breakpoint is on the last block of the last user message"
    );
    for m in &messages[..4] {
        for b in m["content"].as_array().unwrap() {
            assert!(b.get("cache_control").is_none(), "{b}");
        }
    }
    history(&bodies[2]);

    // The trace.
    let store = h.store();
    let trace = store.session_events(&session).unwrap();
    let count = |k: &str| of_kind(&trace, k).len();
    assert_eq!(count(kinds::MENTOR_REQUEST), 3);
    assert_eq!(count(kinds::MENTOR_RESPONSE), 3);
    assert_eq!(count(kinds::ASSISTANT_MESSAGE), 3);
    assert_eq!(count(kinds::TOOL_CALL), 3);
    assert_eq!(count(kinds::TOOL_RESULT), 3);
    assert_eq!(count(kinds::PERMISSION_DECISION), 3);
    assert_eq!(count(kinds::WORKSPACE_SNAPSHOT), 2);
    assert_eq!(count(kinds::OUTCOME), 1);
    assert_eq!(count(kinds::MENTOR_ERROR), 0);
    let kinds = kinds_of(&trace);
    assert_eq!(
        &kinds[..5],
        [
            kinds::SESSION_CREATED,
            kinds::AGENT_STARTED,
            kinds::USER_MESSAGE,
            kinds::WORKSPACE_SNAPSHOT,
            kinds::MENTOR_REQUEST,
        ]
    );
    assert_eq!(
        &kinds[kinds.len() - 6..],
        [
            kinds::MENTOR_REQUEST,
            kinds::MENTOR_RESPONSE,
            kinds::ASSISTANT_MESSAGE,
            kinds::WORKSPACE_SNAPSHOT,
            kinds::OUTCOME,
            kinds::AGENT_FINISHED,
        ]
    );
    let requests = of_kind(&trace, kinds::MENTOR_REQUEST);
    let hashes: Vec<&Value> = requests.iter().map(|r| &r.payload["tool_names"]).collect();
    assert!(hashes.iter().all(|h| *h == hashes[0]));
    assert!(
        requests
            .iter()
            .all(|r| r.payload["prompt_version"] == PROMPT_VERSION),
        "{:?}",
        requests[0].payload
    );
    assert_eq!(
        store.get_session(&session).unwrap().config["prompt_version"],
        PROMPT_VERSION
    );
    assert_eq!(requests[0].payload["message_count"], 1);
    assert_eq!(requests[1].payload["message_count"], 3);
    assert_eq!(requests[2].payload["message_count"], 5);
    // Each request blob is byte for byte what the server received.
    let raw = h.server.received_requests().await.unwrap();
    for (i, r) in requests.iter().enumerate() {
        assert_eq!(blob(store, r), raw[i].body);
    }
    let steps = store.list_steps(&handle.agent_id).unwrap();
    assert_eq!(steps.len(), 3);
    assert_eq!(steps.iter().map(|s| s.seq).collect::<Vec<_>>(), [1, 2, 3]);
    assert!(steps.iter().all(|s| s.status == RunStatus::Ok));
    let step_ids: Vec<Option<&str>> = requests
        .iter()
        .map(|r| r.summary.step_id.as_deref())
        .collect();
    assert_eq!(
        step_ids,
        steps
            .iter()
            .map(|s| Some(s.id.as_str()))
            .collect::<Vec<_>>()
    );
    let calls = of_kind(&trace, kinds::TOOL_CALL);
    assert_eq!(
        calls[2].summary.step_id.as_deref(),
        Some(steps[1].id.as_str())
    );
    let messages = of_kind(&trace, kinds::ASSISTANT_MESSAGE);
    assert_eq!(
        messages[0].payload["tool_calls"],
        json!(["read_file", "read_file"])
    );
    assert_eq!(messages[0].payload["stop_reason"], "tool_use");
    assert_eq!(blob(store, messages[2]), b"Hello, world!");
    let outcome = of_kind(&trace, kinds::OUTCOME)[0];
    assert_eq!(outcome.payload["kind"], "files_changed");
    assert_eq!(outcome.payload["details"]["added"], json!(["src/hello.rs"]));
    let snapshots = of_kind(&trace, kinds::WORKSPACE_SNAPSHOT);
    assert_eq!(snapshots[0].payload["phase"], "start");
    assert_eq!(snapshots[1].payload["phase"], "end");
    assert_eq!(snapshots[0].payload["file_count"], 2);
    assert_eq!(snapshots[1].payload["file_count"], 3);
    assert_eq!(
        store.get_agent(&handle.agent_id).unwrap().status,
        RunStatus::Ok
    );
    h.state.close().await;
}

#[tokio::test]
async fn a_denied_tool_is_an_error_result_and_the_loop_goes_on() {
    let h = Harness::new("").await;
    std::fs::create_dir_all(h.workspace.path().join(".harness")).unwrap();
    std::fs::write(
        h.workspace.path().join(".harness/permissions.toml"),
        "[[rule]]\ntool = \"write_file\"\neffect = \"deny\"\n[rule.match]\npath = \"src/**\"\n",
    )
    .unwrap();
    h.script(vec![sse("tool_use_write"), sse("text")]).await;
    let session = h.session().await;
    let (_, mut rx) = h.run(&session, "add hello", PermissionMode::Auto).await;
    let events = collect(&mut rx).await;
    assert_eq!(finished(&events), (AgentStatus::Ok, None, false));
    assert!(!h.workspace.path().join("src/hello.rs").exists());
    let result = events
        .iter()
        .find_map(|e| match e {
            Event::AgentToolResult { ok, summary, .. } => Some((*ok, summary.clone())),
            _ => None,
        })
        .unwrap();
    assert!(!result.0);
    assert!(result.1.contains("denied"), "{}", result.1);

    // The mentor saw the denial as an error result and answered.
    let bodies = h.requests().await;
    assert_eq!(bodies.len(), 2);
    let result = &bodies[1]["messages"][2]["content"][0];
    assert_eq!(result["type"], "tool_result");
    assert_eq!(result["is_error"], true);
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("denied by rule workspace:1"), "{text}");
    history(&bodies[1]);
    let trace = h.store().session_events(&session).unwrap();
    let decision = of_kind(&trace, kinds::PERMISSION_DECISION)[0];
    assert_eq!(decision.payload["decision"], "deny");
    assert_eq!(decision.payload["rule_ref"], "workspace:1");
    assert_eq!(
        of_kind(&trace, kinds::TOOL_RESULT)[0].payload["kind"],
        "denied"
    );
    assert_eq!(
        of_kind(&trace, kinds::OUTCOME)[0].payload["details"]["counts"]["added"],
        0
    );
    h.state.close().await;
}

#[tokio::test]
async fn cancelling_during_tools_and_during_streaming_leaves_a_valid_history() {
    let h = Harness::new("").await;
    h.script(vec![
        // Run 1: a tool that never ends; cancelled while it runs.
        sse("tool_use_slow"),
        // Run 2: the answer takes too long; cancelled mid-stream.
        sse("text").set_delay(Duration::from_secs(20)),
        // Run 3: the mentor answers over the repaired history.
        sse("text"),
    ])
    .await;
    let session = h.session().await;

    let (handle, mut rx) = h.run(&session, "wait", PermissionMode::Default).await;
    // Cancel once the tool is running.
    loop {
        match tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap()
        {
            Event::AgentStep {
                phase: StepPhase::Tools,
                ..
            } => break,
            Event::AgentFinished { .. } => panic!("finished before the tool ran"),
            _ => {}
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    handle.cancel.cancel();
    let events = collect(&mut rx).await;
    let (status, error, _) = finished(&events);
    assert_eq!(status, AgentStatus::Cancelled);
    assert_eq!(error.unwrap().kind(), Some("cancelled"));
    assert!(events.iter().any(|e| matches!(
        e,
        Event::AgentToolResult { ok: false, summary, .. } if summary.contains("cancelled")
    )));
    let trace = h.store().session_events(&session).unwrap();
    assert_eq!(
        of_kind(&trace, kinds::TOOL_RESULT)[0].payload["kind"],
        "cancelled"
    );
    let steps = h.store().list_steps(&handle.agent_id).unwrap();
    assert_eq!(steps[0].status, RunStatus::Cancelled);

    let (handle, mut rx) = h.run(&session, "and now?", PermissionMode::Default).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    handle.cancel.cancel();
    let events = collect(&mut rx).await;
    assert_eq!(finished(&events).0, AgentStatus::Cancelled);
    // The second request carried the cancelled tool result and the new
    // prompt in one user turn.
    let bodies = h.requests().await;
    assert_eq!(bodies.len(), 2);
    let messages = bodies[1]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[2]["content"][0]["type"], "tool_result");
    assert_eq!(messages[2]["content"][0]["is_error"], true);
    assert_eq!(messages[2]["content"][1]["text"], "and now?");
    history(&bodies[1]);

    // The third run: no partial assistant turn from the cancelled
    // stream, both prompts in the last user message.
    let (_, mut rx) = h
        .run(&session, "still there?", PermissionMode::Default)
        .await;
    let events = collect(&mut rx).await;
    assert_eq!(finished(&events), (AgentStatus::Ok, None, false));
    let bodies = h.requests().await;
    assert_eq!(bodies.len(), 3);
    let messages = bodies[2]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[2]["content"].as_array().unwrap().len(), 3);
    assert_eq!(messages[2]["content"][2]["text"], "still there?");
    assert_eq!(
        messages[2]["content"][2]["cache_control"]["type"],
        "ephemeral"
    );
    history(&bodies[2]);
    h.state.close().await;
}

#[tokio::test]
async fn max_tokens_continues_once_then_ends_truncated() {
    let h = Harness::new("").await;
    h.script(vec![
        sse("max_tokens"),
        sse("max_tokens"),
        // A later run: cut off, then finished.
        sse("max_tokens"),
        sse("text"),
    ])
    .await;
    let session = h.session().await;
    let (handle, mut rx) = h.run(&session, "a story", PermissionMode::Default).await;
    let events = collect(&mut rx).await;
    assert_eq!(finished(&events), (AgentStatus::Ok, None, true));
    let bodies = h.requests().await;
    assert_eq!(bodies.len(), 2);
    let messages = bodies[1]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1]["content"][0]["text"], "Once upon a time there");
    assert_eq!(messages[2]["content"][0]["text"], CONTINUE_MESSAGE);
    let steps = h.store().list_steps(&handle.agent_id).unwrap();
    assert_eq!(steps.len(), 2);
    let trace = h.store().session_events(&session).unwrap();
    let users = of_kind(&trace, kinds::USER_MESSAGE);
    assert_eq!(users.len(), 2);
    assert_eq!(users[1].payload["synthetic"], "continue");
    assert_eq!(
        of_kind(&trace, kinds::ASSISTANT_MESSAGE)[1].payload["truncated"],
        true
    );

    let (_, mut rx) = h.run(&session, "go on", PermissionMode::Default).await;
    let events = collect(&mut rx).await;
    assert_eq!(finished(&events), (AgentStatus::Ok, None, false));
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            Event::AgentTextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Once upon a time thereHello, world!");
    let bodies = h.requests().await;
    assert_eq!(bodies.len(), 4);
    history(&bodies[3]);
    h.state.close().await;
}

#[tokio::test]
async fn the_iteration_guard_stops_a_loop_that_never_ends() {
    let h = Harness::new("\n[runtime]\nmax_iterations = 2\n").await;
    h.script(vec![
        sse("tool_use_parallel"),
        sse("tool_use_parallel"),
        sse("tool_use_parallel"),
    ])
    .await;
    let session = h.session().await;
    let (handle, mut rx) = h
        .run(&session, "read forever", PermissionMode::Default)
        .await;
    let events = collect(&mut rx).await;
    let (status, error, _) = finished(&events);
    assert_eq!(status, AgentStatus::Error);
    let error = error.unwrap();
    assert_eq!(error.kind(), Some("max_iterations"));
    assert!(error.message.contains("after 2 mentor calls"), "{error:?}");
    assert_eq!(h.requests().await.len(), 2);
    let trace = h.store().session_events(&session).unwrap();
    let outcomes = of_kind(&trace, kinds::OUTCOME);
    assert_eq!(outcomes[0].payload["kind"], "error");
    assert_eq!(outcomes[0].payload["details"]["kind"], "max_iterations");
    assert_eq!(outcomes[0].payload["details"]["max_iterations"], 2);
    assert_eq!(outcomes[1].payload["kind"], "files_changed");
    assert_eq!(
        h.store().get_agent(&handle.agent_id).unwrap().status,
        RunStatus::Error
    );
    // Both steps completed; the history is whole.
    let steps = h.store().list_steps(&handle.agent_id).unwrap();
    assert!(steps.iter().all(|s| s.status == RunStatus::Ok));
    h.state.close().await;
}

#[tokio::test]
async fn the_context_guards_warn_then_stop() {
    // The recorded calls use 792 input tokens (412 + 380 cached).
    let h = Harness::new("context_soft_limit = 500\ncontext_hard_limit = 700\n").await;
    h.script(vec![sse("tool_use_parallel"), sse("text")]).await;
    let session = h.session().await;
    let (_, mut rx) = h.run(&session, "read", PermissionMode::Default).await;
    let events = collect(&mut rx).await;
    let (status, error, _) = finished(&events);
    assert_eq!(status, AgentStatus::Error);
    let error = error.unwrap();
    assert_eq!(error.kind(), Some("context_limit"));
    assert!(error.message.contains("792 tokens"), "{error:?}");
    assert_eq!(h.requests().await.len(), 1);
    let warnings: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            Event::AgentWarning { kind, .. } => Some(kind.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        warnings.is_empty(),
        "the stop needs no warning: {warnings:?}"
    );
    let trace = h.store().session_events(&session).unwrap();
    let outcome = of_kind(&trace, kinds::OUTCOME)[0];
    assert_eq!(outcome.payload["details"]["kind"], "context_limit");
    assert_eq!(outcome.payload["details"]["context_tokens"], 792);

    // Under the hard limit the warning goes out once and the run ends.
    let h = Harness::new("context_soft_limit = 500\n").await;
    h.script(vec![
        sse("tool_use_parallel"),
        sse("tool_use_parallel"),
        sse("text"),
    ])
    .await;
    let session = h.session().await;
    let (_, mut rx) = h.run(&session, "read", PermissionMode::Default).await;
    let events = collect(&mut rx).await;
    assert_eq!(finished(&events), (AgentStatus::Ok, None, false));
    let warnings = events
        .iter()
        .filter(|e| matches!(e, Event::AgentWarning { kind, .. } if kind == "context_large"))
        .count();
    assert_eq!(warnings, 1);
    assert_eq!(h.requests().await.len(), 3);
    h.state.close().await;
}

#[tokio::test]
async fn a_refusal_ends_the_run_with_its_category() {
    let h = Harness::new("").await;
    h.script(vec![sse("refusal")]).await;
    let session = h.session().await;
    let (handle, mut rx) = h.run(&session, "do harm", PermissionMode::Default).await;
    let events = collect(&mut rx).await;
    let (status, error, _) = finished(&events);
    assert_eq!(status, AgentStatus::Error);
    let error = error.unwrap();
    assert_eq!(error.kind(), Some("refusal"));
    assert!(error.message.contains("category: cyber"), "{error:?}");
    let trace = h.store().session_events(&session).unwrap();
    let outcome = of_kind(&trace, kinds::OUTCOME)[0];
    assert_eq!(outcome.payload["details"]["kind"], "refusal");
    assert_eq!(outcome.payload["details"]["category"], "cyber");
    let steps = h.store().list_steps(&handle.agent_id).unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].status, RunStatus::Ok);
    h.state.close().await;
}

#[tokio::test]
async fn a_rate_limit_is_waited_out_and_a_broken_stream_retried_once() {
    let h = Harness::new("").await;
    h.script(vec![
        ResponseTemplate::new(429)
            .insert_header("retry-after", "1")
            .set_body_json(json!({
                "type": "error",
                "error": {"type": "rate_limit_error", "message": "slow down"}
            })),
        sse("error_midstream"),
        sse("text"),
    ])
    .await;
    let session = h.session().await;
    let (handle, mut rx) = h.run(&session, "hello", PermissionMode::Default).await;
    let started = std::time::Instant::now();
    let events = collect(&mut rx).await;
    assert!(started.elapsed() >= Duration::from_secs(1));
    assert_eq!(finished(&events), (AgentStatus::Ok, None, false));
    let waits: Vec<(&str, u64)> = events
        .iter()
        .filter_map(|e| match e {
            Event::AgentWaiting {
                reason, wait_ms, ..
            } => Some((reason.as_str(), *wait_ms)),
            _ => None,
        })
        .collect();
    assert_eq!(waits, [("rate_limited", 1000)]);
    let warnings: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            Event::AgentWarning { kind, .. } => Some(kind.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(warnings, ["stream_interrupted"]);
    // The partial text streamed before the break, then the whole
    // answer.
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            Event::AgentTextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "PartialHello, world!");
    assert_eq!(h.requests().await.len(), 3);

    // One step, one call, two non-final errors, then the response.
    let trace = h.store().session_events(&session).unwrap();
    let errors = of_kind(&trace, kinds::MENTOR_ERROR);
    assert_eq!(errors.len(), 2);
    assert_eq!(errors[0].payload["kind"], "rate_limited");
    assert_eq!(errors[0].payload["retry_no"], 1);
    assert_eq!(errors[0].payload["http_status"], 429);
    assert_eq!(errors[1].payload["kind"], "stream_interrupted");
    assert_eq!(errors[1].payload["retry_no"], 2);
    assert_eq!(of_kind(&trace, kinds::MENTOR_REQUEST).len(), 1);
    let response = of_kind(&trace, kinds::MENTOR_RESPONSE)[0];
    assert_eq!(response.payload["call_id"], errors[0].payload["call_id"]);
    assert_eq!(h.store().list_steps(&handle.agent_id).unwrap().len(), 1);

    // Out of budget: no wait, the error is final.
    let h = Harness::new("\n[runtime]\nmax_wait_s = 0\n").await;
    h.script(vec![ResponseTemplate::new(529).set_body_json(json!({
        "type": "error",
        "error": {"type": "overloaded_error", "message": "busy"}
    }))])
    .await;
    let session = h.session().await;
    let (_, mut rx) = h.run(&session, "hello", PermissionMode::Default).await;
    let events = collect(&mut rx).await;
    let (status, error, _) = finished(&events);
    assert_eq!(status, AgentStatus::Error);
    assert_eq!(error.unwrap().kind(), Some("mentor_error"));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::AgentWaiting { .. }))
    );
    let trace = h.store().session_events(&session).unwrap();
    let errors = of_kind(&trace, kinds::MENTOR_ERROR);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].payload["kind"], "overloaded");
    h.state.close().await;
}

#[tokio::test]
async fn a_prompt_goes_through_the_handle_and_headless_runs_are_denied() {
    let h = Harness::new("").await;
    h.script(vec![
        sse("tool_use_write"),
        sse("text"),
        sse("tool_use_write"),
        sse("text"),
    ])
    .await;
    let session = h.session().await;
    let (_, mut rx) = h.run(&session, "add hello", PermissionMode::Default).await;
    let mut asked = None;
    let events = loop {
        let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            Event::PermissionRequest {
                request_id,
                tool,
                paths,
                suggested_rules,
                ..
            } => {
                assert_eq!(tool, "write_file");
                assert_eq!(paths, ["src/hello.rs"]);
                assert!(!suggested_rules.is_empty());
                assert_eq!(h.state.permissions().pending().len(), 1);
                h.state
                    .permissions()
                    .respond(&request_id, PermissionAnswer::AllowOnce, None)
                    .unwrap();
                asked = Some(request_id);
            }
            Event::AgentFinished { .. } => break vec![ev],
            _ => {}
        }
    };
    let asked = asked.expect("a permission request");
    assert_eq!(finished(&events), (AgentStatus::Ok, None, false));
    assert!(h.workspace.path().join("src/hello.rs").is_file());
    let trace = h.store().session_events(&session).unwrap();
    let decision = of_kind(&trace, kinds::PERMISSION_DECISION)[0];
    assert_eq!(decision.payload["decision"], "allow");
    assert_eq!(decision.payload["source"], "user");
    assert_eq!(decision.payload["request_id"], asked);
    assert_eq!(decision.payload["answer"], "allow_once");
    std::fs::remove_file(h.workspace.path().join("src/hello.rs")).unwrap();

    // Nobody listening: the prompt is headless and the write denied.
    let (handle, rx) = h.run(&session, "again", PermissionMode::Default).await;
    drop(rx);
    let done = tokio::time::timeout(Duration::from_secs(10), handle.finished())
        .await
        .unwrap();
    assert!(matches!(
        done,
        Event::AgentFinished {
            status: AgentStatus::Ok,
            ..
        }
    ));
    assert!(!h.workspace.path().join("src/hello.rs").exists());
    let trace = h.store().session_events(&session).unwrap();
    let decision = of_kind(&trace, kinds::PERMISSION_DECISION)[1];
    assert_eq!(decision.payload["decision"], "deny");
    assert_eq!(decision.payload["source"], "headless");
    let bodies = h.requests().await;
    let result = &bodies[3]["messages"][6]["content"][0];
    assert_eq!(result["is_error"], true);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("denied"),
        "{result}"
    );
    history(&bodies[3]);
    h.state.close().await;
}

#[tokio::test]
async fn a_session_runs_one_agent_at_a_time() {
    let h = Harness::new("").await;
    h.script(vec![
        sse("text").set_delay(Duration::from_millis(500)),
        sse("text"),
    ])
    .await;
    let session = h.session().await;
    let (handle, mut rx) = h.run(&session, "one", PermissionMode::Default).await;
    let err = run_agent(
        &h.state,
        session.clone(),
        "two".into(),
        RunOptions::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.kind(), Some("conflict"));
    assert_eq!(
        err.data.unwrap().details.unwrap()["agent_id"],
        handle.agent_id.to_string()
    );
    let events = collect(&mut rx).await;
    assert_eq!(finished(&events), (AgentStatus::Ok, None, false));
    // Free again: the second prompt runs over the same history.
    let (_, mut rx) = h.run(&session, "two", PermissionMode::Default).await;
    let events = collect(&mut rx).await;
    assert_eq!(finished(&events), (AgentStatus::Ok, None, false));
    let bodies = h.requests().await;
    assert_eq!(bodies[1]["messages"].as_array().unwrap().len(), 3);
    assert_eq!(bodies[1]["messages"][2]["content"][0]["text"], "two");
    h.state.close().await;
}
