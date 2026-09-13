//! Outcome signals end to end (task M01-15) against `AppState` with a
//! scripted mentor: `run_tests` finding and running a real `cargo
//! test`, a plain `shell` call yielding the same label, marks from
//! `session.mark` over the router, the stalled-agent watchdog on a
//! hanging tool, agents a dead daemon left running, a reverted run,
//! `stats.outcomes` and `workspace.init`.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apprentice_api::events::{AgentStatus, Event};
use apprentice_api::jsonrpc::{RpcError, codes};
use apprentice_api::methods::{
    SessionCreateParams, SessionGet, SessionGetParams, SessionMarkMethod, SessionMarkParams,
    StatsOutcomes, StatsOutcomesParams, WorkspaceInit, WorkspaceInitParams,
};
use apprentice_api::server::{Router, RouterConfig};
use apprentice_api::types::TraceEvent;
use apprentice_api::types::{PermissionAnswer, PermissionMode, RunOptions, SessionMark};
use apprentice_client::{ClientError, ClientOptions, DaemonClient};
use apprentice_core::app::AppState;
use apprentice_core::config::{ConfigLoader, Paths};
use apprentice_core::runtime::{run_agent, stall_window};
use apprentice_core::secrets::{Secret, SecretStore as _, api_key_name};
use apprentice_core::tools::{Risk, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};
use apprentice_core::trace::{NewAgent, RunStatus, SessionId, TraceStore, kinds};
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

/// A tool that never returns and ignores cancellation: what a stalled
/// run looks like from the outside.
struct Hang;

#[async_trait]
impl Tool for Hang {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "slow",
            "Hangs.",
            json!({"type": "object", "properties": {}}),
            Risk::ReadOnly,
        )
        .with_timeout(Duration::from_secs(3600))
    }

    async fn call(
        &self,
        _: &ToolContext,
        _: Value,
        _: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        std::future::pending().await
    }
}

struct Harness {
    home: tempfile::TempDir,
    workspace: tempfile::TempDir,
    state: Arc<AppState>,
    server: MockServer,
}

impl Harness {
    /// Like the loop tests' harness, with the whole config under the
    /// test's control (`config` is the user config file).
    async fn with_config(config: &str) -> Self {
        let server = MockServer::start().await;
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(home.path());
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        std::fs::write(
            paths.config_file(),
            config.replace("{base_url}", &server.uri()),
        )
        .unwrap();
        let loader = ConfigLoader::new(paths);
        let config = loader.load(None).unwrap().config;
        let state = AppState::open_with(loader, &config).unwrap();
        state
            .secrets()
            .set(&api_key_name("anthropic"), &Secret::new("sk-ant-test"))
            .unwrap();
        let workspace = tempfile::tempdir().unwrap();
        Self {
            home,
            workspace,
            state,
            server,
        }
    }

    async fn new() -> Self {
        Self::with_config(BASE_CONFIG).await
    }

    /// A tiny crate in the workspace: two tests, one ignored.
    fn cargo_project(&self) {
        let root = self.workspace.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
        )
        .unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {\n        assert_eq!(super::add(1, 2), 3);\n    }\n    #[test]\n    fn twice() {\n        assert_eq!(super::add(2, 2), 4);\n    }\n    #[test]\n    #[ignore]\n    fn slow() {}\n}\n",
        )
        .unwrap();
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
                title: Some("outcomes".into()),
            })
            .await
            .unwrap()
            .session_id
            .into()
    }

    fn store(&self) -> &TraceStore {
        self.state.store()
    }

    async fn run(&self, session: &SessionId, prompt: &str) -> (Vec<Event>, String) {
        let (handle, mut rx) = run_agent(
            &self.state,
            session.clone(),
            prompt.into(),
            RunOptions {
                permission_mode: Some(PermissionMode::Auto),
                ..RunOptions::default()
            },
        )
        .await
        .unwrap();
        (
            collect(&self.state, &mut rx).await,
            handle.agent_id.to_string(),
        )
    }

    /// A client talking to every method of the state over a duplex.
    async fn client(&self) -> DaemonClient {
        let mut router = Router::new(RouterConfig {
            daemon_version: "0".into(),
            pid: 1,
            token: None,
        });
        self.state.register(&mut router);
        let (server_side, client_side) = tokio::io::duplex(1 << 16);
        let (sr, sw) = tokio::io::split(server_side);
        let router = Arc::new(router);
        tokio::spawn(async move {
            let _ = router.serve(sr, sw).await;
        });
        let (cr, cw) = tokio::io::split(client_side);
        let client = DaemonClient::from_streams(cr, cw, ClientOptions::default());
        client.hello("test", "0", None).await.unwrap();
        client
    }
}

const BASE_CONFIG: &str = "[daemon]\nsecret_store = \"file\"\n\n[sessions]\nauto_title = false\n\n[mentor]\nbase_url = \"{base_url}\"\nmax_retries = 0\ntimeout_s = 30\n";

/// Collects events until the terminal one, allowing every permission
/// request on the way (execute calls are asked under `auto`).
async fn collect(state: &AppState, rx: &mut broadcast::Receiver<Event>) -> Vec<Event> {
    let mut out = Vec::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(120), rx.recv())
            .await
            .expect("agent finished in time")
            .expect("channel open");
        if let Event::PermissionRequest { request_id, .. } = &ev {
            state
                .permissions()
                .respond(request_id, PermissionAnswer::AllowOnce, None)
                .unwrap();
        }
        let done = ev.is_terminal();
        out.push(ev);
        if done {
            return out;
        }
    }
}

fn finished(events: &[Event]) -> (AgentStatus, Option<&RpcError>) {
    match events.last() {
        Some(Event::AgentFinished { status, error, .. }) => (*status, error.as_ref()),
        other => panic!("expected agent.finished, got {other:?}"),
    }
}

fn outcomes_of(events: &[Event]) -> Vec<(String, String, Option<bool>)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::AgentOutcome {
                kind, summary, ok, ..
            } => Some((kind.clone(), summary.clone(), *ok)),
            _ => None,
        })
        .collect()
}

fn trace_outcomes(store: &TraceStore, session: &SessionId) -> Vec<TraceEvent> {
    store
        .session_events(session)
        .unwrap()
        .into_iter()
        .filter(|e| e.summary.kind == kinds::OUTCOME)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn run_tests_and_a_shell_cargo_test_both_yield_a_tests_outcome() {
    let h = Harness::new().await;
    h.cargo_project();
    h.script(vec![
        sse("tool_use_run_tests"),
        sse("text"),
        sse("tool_use_shell_tests"),
        sse("text"),
    ])
    .await;
    let session = h.session().await;

    // `run_tests {}`: the runner comes from Cargo.toml, the counts from
    // libtest's summary, the mentor reads one parsed line on top.
    let (events, a1) = h.run(&session, "run the tests").await;
    assert_eq!(finished(&events).0, AgentStatus::Ok);
    let outcomes = outcomes_of(&events);
    assert_eq!(
        outcomes[0],
        (
            "tests".into(),
            "2 passed, 1 skipped (cargo test)".into(),
            Some(true)
        ),
        "{outcomes:?}"
    );
    let rows = h.store().session_messages(&session, 0, None).unwrap();
    let result = rows[2].content[0].clone();
    let text = serde_json::to_value(&result).unwrap()["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        text.starts_with("[tests] 2 passed, 1 skipped (cargo test)\n"),
        "{text}"
    );
    assert!(text.contains("test result: ok. 2 passed; 0 failed; 1 ignored"));

    // A plain `shell` call with `cargo test` is read the same way.
    let (events, a2) = h.run(&session, "again with shell").await;
    assert_eq!(finished(&events).0, AgentStatus::Ok);
    let outcomes = outcomes_of(&events);
    assert_eq!(outcomes[0].0, "tests");
    assert_eq!(outcomes[0].1, "2 passed, 1 skipped (cargo test)");

    let recorded = trace_outcomes(h.store(), &session);
    let tests: Vec<&TraceEvent> = recorded
        .iter()
        .filter(|e| e.payload["kind"] == "tests")
        .collect();
    assert_eq!(tests.len(), 2);
    let d = &tests[0].payload["details"];
    assert_eq!(d["runner"], "cargo");
    assert_eq!(d["command"], "cargo test");
    assert_eq!(d["source"], "run_tests");
    assert_eq!(d["parsed"], true);
    assert_eq!(
        (
            d["passed"].as_u64(),
            d["failed"].as_u64(),
            d["skipped"].as_u64()
        ),
        (Some(2), Some(0), Some(1))
    );
    assert_eq!(d["exit_code"], 0);
    assert_eq!(tests[0].summary.agent_id.as_deref(), Some(a1.as_str()));
    assert!(tests[0].summary.step_id.is_some(), "recorded at the step");
    assert_eq!(tests[1].payload["details"]["source"], "shell");
    assert_eq!(tests[1].summary.agent_id.as_deref(), Some(a2.as_str()));
    // The tool result's metadata carries the same facts.
    let result_events: Vec<TraceEvent> = h
        .store()
        .session_events(&session)
        .unwrap()
        .into_iter()
        .filter(|e| e.summary.kind == kinds::TOOL_RESULT)
        .collect();
    assert_eq!(result_events[0].payload["metadata"]["outcome"]["passed"], 2);
    assert_eq!(
        result_events[0].payload["metadata"]["detected_from"],
        "Cargo.toml"
    );

    // `session.get` lists them per run; `stats.outcomes` counts them.
    let client = h.client().await;
    let got = client
        .call::<SessionGet>(SessionGetParams {
            id: session.to_string(),
            after_seq: None,
            before_seq: None,
            limit: None,
        })
        .await
        .unwrap();
    assert_eq!(got.agents.len(), 2);
    assert_eq!(
        got.agents[0]
            .outcomes
            .iter()
            .map(|o| o.kind.as_str())
            .collect::<Vec<_>>(),
        ["tests", "files_changed"]
    );
    assert_eq!(got.agents[0].outcomes[0].ok, Some(true));
    assert!(got.agents[0].error.is_none());
    let stats = client
        .call::<StatsOutcomes>(StatsOutcomesParams::default())
        .await
        .unwrap();
    assert_eq!((stats.agents, stats.labelled), (2, 2));
    assert!((stats.labelled_share - 1.0).abs() < f64::EPSILON);
    assert_eq!(stats.tests_passed, 2);
    assert_eq!(stats.by_kind["tests"], 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn marks_land_on_the_right_agent_and_count_as_labels() {
    let h = Harness::new().await;
    h.script(vec![sse("text"), sse("text")]).await;
    let session = h.session().await;
    let client = h.client().await;

    // No run yet: nothing to mark.
    let err = client
        .call::<SessionMarkMethod>(SessionMarkParams {
            id: session.to_string(),
            mark: SessionMark::Accept,
            note: None,
            agent_id: None,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == codes::NOT_FOUND),
        "{err}"
    );

    let (_, a1) = h.run(&session, "one").await;
    let (_, a2) = h.run(&session, "two").await;
    let r = client
        .call::<SessionMarkMethod>(SessionMarkParams {
            id: session.to_string(),
            mark: SessionMark::Accept,
            note: Some("  looks right ".into()),
            agent_id: None,
        })
        .await
        .unwrap();
    assert_eq!(r.agent_id, a2, "the last run by default");
    assert_eq!(r.outcome.kind, "user_accept");
    assert_eq!(r.outcome.summary, "accepted: looks right");
    assert_eq!(r.outcome.ok, Some(true));
    assert_eq!(r.outcome.details["note"], "looks right");
    let r = client
        .call::<SessionMarkMethod>(SessionMarkParams {
            id: session.to_string(),
            mark: SessionMark::Reject,
            note: None,
            agent_id: Some(a1.clone()),
        })
        .await
        .unwrap();
    assert_eq!(
        (r.agent_id.as_str(), r.outcome.kind.as_str()),
        (a1.as_str(), "user_reject")
    );
    assert_eq!(r.outcome.details.get("note"), None);
    client
        .call::<SessionMarkMethod>(SessionMarkParams {
            id: session.to_string(),
            mark: SessionMark::Done,
            note: None,
            agent_id: None,
        })
        .await
        .unwrap();
    let err = client
        .call::<SessionMarkMethod>(SessionMarkParams {
            id: session.to_string(),
            mark: SessionMark::Done,
            note: None,
            agent_id: Some("nope".into()),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == codes::INVALID_PARAMS),
        "{err}"
    );

    let got = client
        .call::<SessionGet>(SessionGetParams {
            id: session.to_string(),
            after_seq: None,
            before_seq: None,
            limit: None,
        })
        .await
        .unwrap();
    let kinds_of = |i: usize| {
        got.agents[i]
            .outcomes
            .iter()
            .map(|o| o.kind.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(kinds_of(0), ["files_changed", "user_reject"]);
    assert_eq!(kinds_of(1), ["files_changed", "user_accept", "task_done"]);
    let recorded = trace_outcomes(h.store(), &session);
    let marks: Vec<(&str, &str)> = recorded
        .iter()
        .filter(|e| {
            e.payload["kind"].as_str().unwrap().starts_with("user_")
                || e.payload["kind"] == "task_done"
        })
        .map(|e| {
            (
                e.summary.agent_id.as_deref().unwrap(),
                e.payload["kind"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        marks,
        [
            (a2.as_str(), "user_accept"),
            (a1.as_str(), "user_reject"),
            (a2.as_str(), "task_done")
        ]
    );

    let stats = client
        .call::<StatsOutcomes>(StatsOutcomesParams::default())
        .await
        .unwrap();
    assert_eq!(
        (
            stats.agents,
            stats.labelled,
            stats.accepted,
            stats.rejected,
            stats.done
        ),
        (2, 2, 1, 1, 1)
    );
    assert_eq!(stats.by_kind["files_changed"], 2);
    // A range with nothing in it.
    let none = client
        .call::<StatsOutcomes>(StatsOutcomesParams {
            until: Some("2000-01-01".into()),
            ..StatsOutcomesParams::default()
        })
        .await
        .unwrap();
    assert_eq!(
        (none.agents, none.labelled, none.labelled_share),
        (0, 0, 0.0)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_watchdog_ends_a_silent_run_as_stalled() {
    let config = format!(
        "{BASE_CONFIG}\n[tools.timeout_s]\nexecute = 2\n\n[permissions]\nask_timeout_s = 2\n\n[runtime]\nstall_grace_s = 1\n"
    )
    .replace("timeout_s = 30", "timeout_s = 2");
    let h = Harness::with_config(&config).await;
    h.state.tools().register(Arc::new(Hang)).unwrap();
    let cfg = h.state.loader().load(None).unwrap().config;
    assert_eq!(stall_window(&cfg), Duration::from_secs(3));
    h.script(vec![sse("tool_use_slow")]).await;
    let session = h.session().await;

    let started = std::time::Instant::now();
    let (events, agent) = h.run(&session, "hang").await;
    let (status, error) = finished(&events);
    assert_eq!(status, AgentStatus::Error);
    let error = error.unwrap();
    assert_eq!(error.kind(), Some("stalled"));
    assert!(
        error.message.contains("no sign of life for 3 s"),
        "{}",
        error.message
    );
    assert_eq!(
        error.data.as_ref().unwrap().details.as_ref().unwrap()["window_s"],
        3
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(3) && elapsed < Duration::from_secs(20),
        "{elapsed:?}"
    );
    let outcomes = outcomes_of(&events);
    assert_eq!(
        outcomes[0],
        ("error".into(), "error: stalled".into(), Some(false))
    );

    let record = h.store().get_agent(&agent.as_str().into()).unwrap();
    assert_eq!(record.status, RunStatus::Error);
    let recorded = trace_outcomes(h.store(), &session);
    assert_eq!(recorded[0].payload["details"]["kind"], "stalled");
    // The hanging tool call ended as cancelled in the trace.
    let results: Vec<TraceEvent> = h
        .store()
        .session_events(&session)
        .unwrap()
        .into_iter()
        .filter(|e| e.summary.kind == kinds::TOOL_RESULT)
        .collect();
    assert_eq!(results[0].payload["kind"], "cancelled");
    // The next run on the session works: the history was repaired.
    h.script(vec![sse("text")]).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_a_dead_daemon_left_running_are_ended_at_open() {
    let h = Harness::new().await;
    let session = h.session().await;
    let agent = h
        .store()
        .start_agent(&NewAgent::main(session.clone(), "never finished"))
        .unwrap();
    let step = h.store().start_step(&agent).unwrap();
    h.state.close().await;

    // A new daemon on the same home.
    let paths = Paths::from_home(h.home.path());
    let loader = ConfigLoader::new(paths);
    let config = loader.load(None).unwrap().config;
    let state = AppState::open_with(loader, &config).unwrap();
    let record = state.store().get_agent(&agent).unwrap();
    assert_eq!(record.status, RunStatus::Error);
    assert!(record.ended_at.is_some());
    assert_eq!(
        state.store().get_step(&step).unwrap().status,
        RunStatus::Error
    );
    let events = state.store().session_events(&session).unwrap();
    let finished = events
        .iter()
        .find(|e| e.summary.kind == kinds::AGENT_FINISHED)
        .unwrap();
    assert_eq!(finished.payload["status"], "error");
    assert_eq!(finished.payload["error"]["data"]["kind"], "daemon_restart");
    let outcome = events
        .iter()
        .find(|e| e.summary.kind == kinds::OUTCOME)
        .unwrap();
    assert_eq!(outcome.payload["details"]["kind"], "daemon_restart");
    assert_eq!(outcome.summary.agent_id.as_deref(), Some(agent.as_str()));
    let agents = state.store().session_agents(&session).unwrap();
    assert_eq!(agents[0].status, "error");
    let err = agents[0].error.as_ref().unwrap();
    assert_eq!(err.kind(), Some("daemon_restart"));
    assert_eq!(
        agents[0].outcomes[0].summary,
        "error: daemon_restart — the daemon stopped while the agent was running"
    );
    assert_eq!(
        state
            .store()
            .get_session(&session)
            .unwrap()
            .last_agent_status,
        Some(RunStatus::Error)
    );
    // Opening again finds nothing to do.
    assert!(state.store().recover_orphaned_agents().unwrap().is_empty());
    state.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_whose_file_was_removed_is_marked_reverted_by_the_next() {
    let h = Harness::new().await;
    std::fs::create_dir_all(h.workspace.path().join("src")).unwrap();
    h.script(vec![sse("tool_use_write"), sse("text"), sse("text")])
        .await;
    let session = h.session().await;
    let (events, a1) = h.run(&session, "add hello").await;
    assert_eq!(finished(&events).0, AgentStatus::Ok);
    assert_eq!(
        outcomes_of(&events),
        [("files_changed".into(), "1 file added".into(), None)]
    );
    assert!(h.workspace.path().join("src/hello.rs").is_file());

    // The user throws the file away, then asks again.
    std::fs::remove_file(h.workspace.path().join("src/hello.rs")).unwrap();
    let (events, a2) = h.run(&session, "and now?").await;
    assert_eq!(finished(&events).0, AgentStatus::Ok);
    let reverted = events
        .iter()
        .find_map(|e| match e {
            Event::AgentOutcome {
                agent_id,
                kind,
                summary,
                details,
                ok,
                ..
            } if kind == "reverted" => {
                Some((agent_id.clone(), summary.clone(), details.clone(), *ok))
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(reverted.0, a1, "the outcome is the previous run's");
    assert_eq!(reverted.1, "1 file reverted");
    assert_eq!(reverted.2["files"], json!(["src/hello.rs"]));
    assert_eq!(reverted.2["at_agent"], a2);
    assert_eq!(reverted.3, Some(false));
    let recorded = trace_outcomes(h.store(), &session);
    let rev = recorded
        .iter()
        .find(|e| e.payload["kind"] == "reverted")
        .unwrap();
    assert_eq!(rev.summary.agent_id.as_deref(), Some(a1.as_str()));
    let agents = h.store().session_agents(&session).unwrap();
    assert_eq!(
        agents[0]
            .outcomes
            .iter()
            .map(|o| o.kind.as_str())
            .collect::<Vec<_>>(),
        ["files_changed", "reverted"]
    );

    // A third run finds the previous (second) run changed nothing:
    // no revert to report.
    h.script(vec![sse("text")]).await;
    let (events, _) = h.run(&session, "still there?").await;
    assert!(outcomes_of(&events).iter().all(|(k, _, _)| k != "reverted"));
}

#[tokio::test(flavor = "multi_thread")]
async fn workspace_init_writes_the_template_once() {
    let h = Harness::new().await;
    let session = h.session().await;
    let ws_id = h
        .store()
        .get_session(&session)
        .unwrap()
        .workspace_id
        .unwrap()
        .to_string();
    let client = h.client().await;
    let r = client
        .call::<WorkspaceInit>(WorkspaceInitParams {
            id: ws_id.clone(),
            force: false,
        })
        .await
        .unwrap();
    let path = Path::new(&r.path);
    assert!(path.is_file());
    assert!(!r.replaced);
    let text = std::fs::read_to_string(path).unwrap();
    let name = h.workspace.path().file_name().unwrap().to_string_lossy();
    assert!(text.starts_with(&format!("# {name}\n")), "{text}");
    assert!(text.contains("## How to work here"));
    let err = client
        .call::<WorkspaceInit>(WorkspaceInitParams {
            id: ws_id.clone(),
            force: false,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.code == codes::CONFLICT),
        "{err}"
    );
    std::fs::write(path, "mine\n").unwrap();
    let r = client
        .call::<WorkspaceInit>(WorkspaceInitParams {
            id: ws_id,
            force: true,
        })
        .await
        .unwrap();
    assert!(r.replaced);
    assert!(
        std::fs::read_to_string(path)
            .unwrap()
            .contains("## Pitfalls")
    );
}
