//! Session persistence (task M01-10) against `AppState` with a wiremock
//! mentor: the write-through of every message, the reload after the
//! daemon forgets a conversation (a restart), the repair of a crash
//! mid-tools, search and the session list, archive/rename/delete, the
//! export round trip, the tool-set note on resume, and the title the
//! cheap model generates.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use apprentice_api::events::{AgentStatus, Event};
use apprentice_api::methods::{
    SessionArchiveParams, SessionCreateParams, SessionDeleteParams, SessionGetParams,
    SessionIdParams, SessionListParams, SessionRenameParams, SessionSearchParams,
};
use apprentice_api::types::{PermissionMode, RunOptions, SESSION_EXPORT_FORMAT};
use apprentice_core::app::AppState;
use apprentice_core::config::{ConfigLoader, Paths};
use apprentice_core::mentor::{ContentBlock, MentorRequest, Role};
use apprentice_core::runtime::{AgentHandle, Conversation, PROMPT_VERSION, run_agent};
use apprentice_core::secrets::{Secret, SecretStore as _, api_key_name};
use apprentice_core::sessions;
use apprentice_core::tools::{Risk, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};
use apprentice_core::trace::{
    CallFilter, CallKind, NewMessage, RunStatus, SessionId, SessionStatus, TitleSource, TraceStore,
    kinds,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const TITLE_MODEL: &str = "claude-haiku-4-5-20251001";

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

fn sse(name: &str) -> ResponseTemplate {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sse")
        .join(format!("{name}.txt"));
    let body = std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
}

type Queue = Arc<Mutex<VecDeque<ResponseTemplate>>>;

/// Plays the queued responses in order; a request past the end gets a
/// 500. One mock per server, fed by [`Harness::script`].
struct Script(Queue);

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

struct Harness {
    home: tempfile::TempDir,
    workspace: tempfile::TempDir,
    state: Arc<AppState>,
    server: MockServer,
    queue: Queue,
}

impl Harness {
    /// A state whose mentor talks to a wiremock server, with the title
    /// model answering from `title.txt` when `auto_title` is on.
    async fn new(auto_title: bool) -> Self {
        let server = MockServer::start().await;
        let queue: Queue = Arc::default();
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(Script(Arc::clone(&queue)))
            .mount(&server)
            .await;
        if auto_title {
            Mock::given(method("POST"))
                .and(path("/v1/messages"))
                .and(body_partial_json(json!({ "model": TITLE_MODEL })))
                .respond_with(sse("title"))
                .with_priority(1)
                .mount(&server)
                .await;
        }
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(home.path());
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        std::fs::write(paths.config_file(), config(&server.uri(), auto_title, "")).unwrap();
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
            home,
            workspace,
            state,
            server,
            queue,
        }
    }

    /// Rewrites the user config with `extra` appended.
    fn reconfigure(&self, extra: &str) {
        let paths = Paths::from_home(self.home.path());
        std::fs::write(
            paths.config_file(),
            config(&self.server.uri(), false, extra),
        )
        .unwrap();
    }

    /// Queues responses for the next calls.
    fn script(&self, responses: Vec<ResponseTemplate>) {
        self.queue.lock().unwrap().extend(responses);
    }

    async fn session(&self, title: Option<&str>) -> SessionId {
        self.state
            .session_create(&SessionCreateParams {
                workspace: Some(self.workspace.path().to_string_lossy().into_owned()),
                title: title.map(str::to_owned),
            })
            .await
            .unwrap()
            .session_id
            .into()
    }

    fn store(&self) -> &TraceStore {
        self.state.store()
    }

    async fn run(&self, session: &SessionId, prompt: &str) -> AgentStatus {
        let (handle, mut rx) = self.start(session, prompt).await.unwrap();
        collect(&mut rx).await;
        handle.status().unwrap()
    }

    async fn start(
        &self,
        session: &SessionId,
        prompt: &str,
    ) -> Result<(AgentHandle, broadcast::Receiver<Event>), apprentice_api::jsonrpc::RpcError> {
        run_agent(
            &self.state,
            session.clone(),
            prompt.into(),
            RunOptions {
                permission_mode: Some(PermissionMode::Auto),
                ..RunOptions::default()
            },
        )
        .await
    }

    /// The completion bodies the server received, in order (a resumed
    /// session also asks `count_tokens` for its context size; that one
    /// is not a completion).
    async fn requests(&self) -> Vec<Value> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/v1/messages")
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect()
    }

    /// The three-step trajectory on a fresh untitled session.
    async fn three_steps(&self) -> SessionId {
        self.script(vec![
            sse("tool_use_parallel"),
            sse("tool_use_write"),
            sse("text"),
        ]);
        let session = self.session(None).await;
        assert_eq!(self.run(&session, "add hello").await, AgentStatus::Ok);
        session
    }

    /// What a daemon restart does to the in-memory state.
    fn restart(&self, session: &SessionId) {
        self.state.agents().forget_conversation(session);
    }

    /// Polls until `f` holds or `timeout` passes.
    async fn until(&self, timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while !f() {
            if Instant::now() > deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        true
    }
}

fn config(base_url: &str, auto_title: bool, extra: &str) -> String {
    format!(
        "[daemon]\nsecret_store = \"file\"\n\n[sessions]\nauto_title = {auto_title}\n\n\
         [mentor]\nbase_url = \"{base_url}\"\nmax_retries = 0\ntimeout_s = 30\n\
         title_model = \"{TITLE_MODEL}\"\n{extra}"
    )
}

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

/// `value` without its `cache_control` keys.
fn strip_cache(value: Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .filter(|(k, _)| k != "cache_control")
                .map(|(k, v)| (k, strip_cache(v)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(strip_cache).collect()),
        other => other,
    }
}

fn roles(store: &TraceStore, session: &SessionId) -> Vec<Role> {
    store
        .session_messages(session, 0, None)
        .unwrap()
        .into_iter()
        .map(|m| m.role)
        .collect()
}

fn event_kinds(store: &TraceStore, session: &SessionId) -> Vec<String> {
    store
        .session_events(session)
        .unwrap()
        .into_iter()
        .map(|e| e.summary.kind)
        .collect()
}

#[tokio::test]
async fn every_message_is_stored_as_sent_and_the_session_resumes_after_a_restart() {
    let h = Harness::new(false).await;
    let session = h.three_steps().await;
    let store = h.store();

    // Six rows, one per message, each the blocks the API saw.
    let rows = store.session_messages(&session, 0, None).unwrap();
    assert_eq!(
        rows.iter().map(|m| m.role).collect::<Vec<_>>(),
        [
            Role::User,
            Role::Assistant,
            Role::User,
            Role::Assistant,
            Role::User,
            Role::Assistant
        ]
    );
    assert_eq!(
        rows.iter().map(|m| m.seq).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 6]
    );
    assert_eq!(rows[0].content, vec![ContentBlock::text("add hello")]);
    assert!(rows[0].agent_id.is_some() && rows[0].step_id.is_none());
    assert!(rows[1].step_id.is_some());
    // Byte for byte what the third request carried (breakpoints aside,
    // which are placed per request, never stored).
    let requests = h.requests().await;
    let sent = requests[2]["messages"].as_array().unwrap();
    assert_eq!(sent.len(), 5);
    for (i, m) in sent.iter().enumerate() {
        assert_eq!(
            json!(rows[i].content),
            strip_cache(m["content"].clone()),
            "message {i}"
        );
    }
    let record = store.get_session(&session).unwrap();
    assert_eq!(record.message_count, 6);
    assert_eq!(record.last_agent_status, Some(RunStatus::Ok));
    assert_eq!(record.prompt_version.as_deref(), Some(PROMPT_VERSION));
    assert_eq!(record.title.as_deref(), Some("add hello"));
    assert_eq!(record.title_source, Some(TitleSource::Prompt));
    let tools_hash = record
        .tools_hash
        .clone()
        .expect("recorded at the first run");

    // The list and the info.
    let list = sessions::list(&h.state, &SessionListParams::default())
        .await
        .unwrap();
    assert_eq!(list.sessions.len(), 1);
    let s = &list.sessions[0];
    assert_eq!(s.message_count, 6);
    assert_eq!(s.last_agent_status, Some(AgentStatus::Ok));
    assert_eq!(s.usage.input_tokens, 412 + 650 + 25);
    assert!(s.cost_usd.unwrap() > 0.0);
    assert!(s.last_activity >= s.created_at);
    let got = sessions::get(
        &h.state,
        &SessionGetParams {
            id: session.to_string(),
            after_seq: Some(4),
            limit: Some(1),
        },
    )
    .await
    .unwrap();
    assert_eq!(got.session.tools_hash.as_deref(), Some(tools_hash.as_str()));
    assert_eq!(got.messages.len(), 1);
    assert_eq!(got.messages[0].seq, 5);
    assert!(got.has_more);

    // A restart: the next run loads the six rows and sends them all,
    // with the new prompt appended, and no repair or prefix change.
    h.restart(&session);
    assert!(h.state.agents().loaded_conversation(&session).is_none());
    h.script(vec![sse("text")]);
    assert_eq!(h.run(&session, "thanks").await, AgentStatus::Ok);
    let requests = h.requests().await;
    assert_eq!(requests.len(), 4);
    let fourth: MentorRequest = serde_json::from_value(requests[3].clone()).unwrap();
    assert_eq!(fourth.messages.len(), 7);
    assert_eq!(
        fourth.messages[6].content[0].as_text(),
        Some("thanks"),
        "{:?}",
        fourth.messages[6]
    );
    assert_eq!(fourth.messages[4].content.len(), 1);
    assert_eq!(
        requests[3]["tools"],
        requests[0]["tools"],
        "{}",
        serde_json::to_string(&requests[3]).unwrap()
    );
    Conversation::load(session.clone(), fourth.messages)
        .validate()
        .unwrap();
    assert_eq!(store.get_session(&session).unwrap().message_count, 8);
    let kinds = event_kinds(store, &session);
    assert!(
        !kinds.iter().any(|k| k == kinds::SESSION_REPAIRED),
        "{kinds:?}"
    );
    assert!(
        !kinds.iter().any(|k| k == kinds::SESSION_PREFIX_CHANGED),
        "{kinds:?}"
    );
    // The totals were seeded from the store: the second run's usage
    // event carries the session's running sum.
    let conv = h.state.agents().loaded_conversation(&session).unwrap();
    let conv = conv.lock().await;
    assert_eq!(conv.totals().input_tokens, 412 + 650 + 25 + 25);
    assert!(conv.cost_micros().unwrap() > 0);
    h.state.close().await;
}

#[tokio::test]
async fn a_crash_between_a_call_and_its_tools_is_repaired_on_load() {
    let h = Harness::new(false).await;
    let session = h.three_steps().await;
    let store = h.store();
    // The daemon died after storing an assistant turn with a tool call
    // and before any result.
    store
        .put_session_message(&NewMessage {
            session: session.clone(),
            seq: 7,
            role: Role::Assistant,
            content: vec![
                ContentBlock::text("one more"),
                ContentBlock::ToolUse {
                    id: "toolu_09".into(),
                    name: "read_file".into(),
                    input: json!({"path": "Cargo.toml"}),
                    cache: apprentice_core::mentor::CacheFlag(false),
                },
            ],
            agent: None,
            step: None,
        })
        .unwrap();
    assert_eq!(store.get_session(&session).unwrap().message_count, 7);
    h.restart(&session);

    let conv = h.state.conversation(&session).await.unwrap();
    assert_eq!(conv.lock().await.message_count(), 6);
    assert_eq!(roles(store, &session).len(), 6);
    assert_eq!(store.get_session(&session).unwrap().message_count, 6);
    let repaired = store
        .session_events(&session)
        .unwrap()
        .into_iter()
        .find(|e| e.summary.kind == kinds::SESSION_REPAIRED)
        .expect("session.repaired");
    assert_eq!(repaired.payload["dropped_seq"], 7);
    assert_eq!(repaired.payload["tool_use_ids"], json!(["toolu_09"]));

    // The run after carries the repaired history.
    h.script(vec![sse("text")]);
    assert_eq!(h.run(&session, "go on").await, AgentStatus::Ok);
    let requests = h.requests().await;
    let last: MentorRequest = serde_json::from_value(requests[3].clone()).unwrap();
    assert_eq!(last.messages.len(), 7);
    Conversation::load(session.clone(), last.messages)
        .validate()
        .unwrap();

    // A history no repair can fix is refused, not sent.
    let other = h.session(None).await;
    store
        .put_session_message(&NewMessage {
            session: other.clone(),
            seq: 1,
            role: Role::User,
            content: vec![ContentBlock::tool_result("toolu_none", "?", false)],
            agent: None,
            step: None,
        })
        .unwrap();
    let err = h.start(&other, "hi").await.unwrap_err();
    assert!(
        err.message.contains("not one the API accepts"),
        "{}",
        err.message
    );
    h.state.close().await;
}

#[tokio::test]
async fn search_finds_words_and_the_lifecycle_methods_hide_rename_and_delete() {
    let h = Harness::new(false).await;
    let session = h.three_steps().await;
    let store = h.store();

    // A word from the assistant's answer, with its snippet.
    let hits = store.search_sessions("hello world", false, None).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].session_id, session.as_str());
    assert_eq!((hits[0].seq, hits[0].role.as_str()), (6, "assistant"));
    assert!(
        hits[0].snippet.contains("[Hello], [world]"),
        "{}",
        hits[0].snippet
    );
    // A word from the prompt; prefixes; the query syntax cannot break it.
    assert_eq!(store.search_sessions("hel", false, None).unwrap().len(), 2);
    assert!(
        store
            .search_sessions("nothing_here", false, None)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .search_sessions("\"OR (", false, None)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .search_sessions("   ", false, None)
            .unwrap()
            .is_empty()
    );
    let via_rpc = sessions::search(
        &h.state,
        &SessionSearchParams {
            query: "world".into(),
            include_archived: false,
            limit: Some(5),
        },
    )
    .await
    .unwrap();
    assert_eq!(via_rpc.hits.len(), 1);
    // The list's `query` matches titles and messages.
    let by_query = |q: &str| {
        let state = Arc::clone(&h.state);
        let q = q.to_owned();
        async move {
            sessions::list(
                &state,
                &SessionListParams {
                    query: Some(q),
                    ..SessionListParams::default()
                },
            )
            .await
            .unwrap()
            .sessions
            .len()
        }
    };
    assert_eq!(by_query("world").await, 1);
    assert_eq!(by_query("ADD").await, 1, "the title, case-insensitively");
    assert_eq!(by_query("zzz").await, 0);

    // Archived: hidden from the list and the search unless asked.
    sessions::archive(
        &h.state,
        &SessionArchiveParams {
            id: session.to_string(),
            archived: true,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        store.get_session(&session).unwrap().status,
        SessionStatus::Archived
    );
    assert!(h.state.agents().loaded_conversation(&session).is_none());
    let visible = sessions::list(&h.state, &SessionListParams::default())
        .await
        .unwrap();
    assert!(visible.sessions.is_empty());
    let all = sessions::list(
        &h.state,
        &SessionListParams {
            include_archived: true,
            ..SessionListParams::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(all.sessions[0].status, "archived");
    assert!(
        store
            .search_sessions("world", false, None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.search_sessions("world", true, None).unwrap().len(), 1);
    sessions::archive(
        &h.state,
        &SessionArchiveParams {
            id: session.to_string(),
            archived: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        store.get_session(&session).unwrap().status,
        SessionStatus::Open
    );

    // Renamed by the user: the source says so, the event too.
    sessions::rename(
        &h.state,
        &SessionRenameParams {
            id: session.to_string(),
            title: "  My session  ".into(),
        },
    )
    .await
    .unwrap();
    let record = store.get_session(&session).unwrap();
    assert_eq!(record.title.as_deref(), Some("My session"));
    assert_eq!(record.title_source, Some(TitleSource::User));
    assert!(
        !store
            .set_session_title(&session, Some("Generated"), TitleSource::Generated)
            .unwrap(),
        "a generated title never replaces the user's"
    );
    assert_eq!(
        store.get_session(&session).unwrap().title.as_deref(),
        Some("My session")
    );
    let err = sessions::rename(
        &h.state,
        &SessionRenameParams {
            id: session.to_string(),
            title: " ".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.kind(), Some("invalid_params"));

    // Busy: the mutations refuse while an agent runs.
    h.script(vec![sse("tool_use_slow")]);
    let (handle, mut rx) = h.start(&session, "wait").await.unwrap();
    assert!(
        h.until(Duration::from_secs(10), || handle.is_running()
            && store.get_session(&session).unwrap().message_count >= 8)
            .await
    );
    let busy = sessions::delete(
        &h.state,
        &SessionDeleteParams {
            id: session.to_string(),
            purge_traces: false,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(busy.kind(), Some("conflict"));
    handle.cancel.cancel();
    collect(&mut rx).await;

    // Deleted without purge: messages gone, traces and the row kept.
    let events_before = store.event_count(&session).unwrap();
    let report = sessions::delete(
        &h.state,
        &SessionDeleteParams {
            id: session.to_string(),
            purge_traces: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(report.messages_deleted, 9, "{report:?}");
    assert_eq!(report.events_deleted, 0);
    let record = store.get_session(&session).unwrap();
    assert_eq!(record.status, SessionStatus::Deleted);
    assert_eq!(record.message_count, 0);
    assert!(roles(store, &session).is_empty());
    assert_eq!(store.event_count(&session).unwrap(), events_before + 1);
    assert!(
        store
            .search_sessions("world", true, None)
            .unwrap()
            .is_empty()
    );
    let err = h.start(&session, "again").await.unwrap_err();
    assert_eq!(err.kind(), Some("conflict"));

    // Purged: everything of the session is gone.
    let report = sessions::delete(
        &h.state,
        &SessionDeleteParams {
            id: session.to_string(),
            purge_traces: true,
        },
    )
    .await
    .unwrap();
    assert!(report.events_deleted > 10, "{report:?}");
    assert_eq!(store.get_session(&session).unwrap_err().kind(), "not_found");
    assert!(
        store
            .list_mentor_calls(&CallFilter::session_of(&session))
            .unwrap()
            .is_empty()
    );
    assert!(store.list_agents(&session).unwrap().is_empty());
    let all = sessions::list(
        &h.state,
        &SessionListParams {
            include_archived: true,
            ..SessionListParams::default()
        },
    )
    .await
    .unwrap();
    assert!(all.sessions.is_empty());
    h.state.close().await;
}

#[tokio::test]
async fn an_export_round_trips_into_a_fresh_store() {
    let h = Harness::new(false).await;
    let session = h.three_steps().await;
    let export = sessions::export(
        &h.state,
        &SessionIdParams {
            id: session.to_string(),
        },
    )
    .await
    .unwrap();
    assert_eq!(export.format, SESSION_EXPORT_FORMAT);
    assert_eq!(export.messages.len(), 6);
    assert_eq!(export.mentor_calls.len(), 3);
    assert_eq!(export.session.summary.id, session.as_str());
    assert_eq!(
        export.session.prompt_version.as_deref(),
        Some(PROMPT_VERSION)
    );
    let text = serde_json::to_string_pretty(&export).unwrap();

    let other = tempfile::tempdir().unwrap();
    let fresh = TraceStore::open_at(
        other.path().join("traces.sqlite"),
        other.path().join("blobs"),
        1 << 20,
    )
    .unwrap();
    let parsed = serde_json::from_str(&text).unwrap();
    let id = fresh.import_session(&parsed).unwrap();
    assert_eq!(id, session);
    assert!(matches!(
        fresh.import_session(&parsed).unwrap_err(),
        apprentice_core::trace::TraceError::Conflict(_)
    ));

    let again = fresh.export_session(&session).unwrap();
    assert_eq!(again.messages, export.messages);
    assert_eq!(again.mentor_calls, export.mentor_calls);
    let (mut a, mut b) = (again.session.clone(), export.session.clone());
    a.summary.updated_at = String::new();
    b.summary.updated_at = String::new();
    a.summary.workspace_id = None;
    b.summary.workspace_id = None;
    assert_eq!(a, b);
    assert_eq!(fresh.get_session(&session).unwrap().message_count, 6);
    let usage = fresh.stats(&CallFilter::session_of(&session)).unwrap();
    assert_eq!(usage.calls, 3);
    assert_eq!(usage.input_tokens, 412 + 650 + 25);
    let rows = fresh.session_messages(&session, 0, None).unwrap();
    Conversation::load(
        session.clone(),
        rows.into_iter()
            .map(|m| apprentice_core::mentor::Message {
                role: m.role,
                content: m.content,
            })
            .collect(),
    )
    .validate()
    .unwrap();
    assert_eq!(
        fresh.search_sessions("hello", false, None).unwrap().len(),
        2
    );
    h.state.close().await;
}

#[tokio::test]
async fn resuming_with_a_changed_tool_set_tells_the_mentor_and_records_it() {
    let h = Harness::new(false).await;
    let session = h.three_steps().await;
    let before = h.store().get_session(&session).unwrap();
    h.restart(&session);
    h.reconfigure("\n[tools]\ndisabled = [\"write_file\"]\n");

    h.script(vec![sse("text")]);
    let (handle, mut rx) = h.start(&session, "thanks").await.unwrap();
    let events = collect(&mut rx).await;
    assert_eq!(handle.status(), Some(AgentStatus::Ok));
    assert!(events.iter().any(|e| matches!(
        e,
        Event::AgentWarning { kind, .. } if kind == "tools_changed"
    )));
    let requests = h.requests().await;
    let last: MentorRequest = serde_json::from_value(requests[3].clone()).unwrap();
    assert!(!last.tools.iter().any(|t| t.name == "write_file"));
    let user = &last.messages[6];
    assert_eq!(user.role, Role::User);
    assert_eq!(user.content[0].as_text(), Some("thanks"));
    assert_eq!(
        user.content[1].as_text(),
        Some("[note: tool set changed since this session started]")
    );
    let after = h.store().get_session(&session).unwrap();
    assert_ne!(after.tools_hash, before.tools_hash);
    assert_eq!(after.prompt_version, before.prompt_version);
    let changed = h
        .store()
        .session_events(&session)
        .unwrap()
        .into_iter()
        .find(|e| e.summary.kind == kinds::SESSION_PREFIX_CHANGED)
        .expect("session.prefix_changed");
    assert_eq!(
        changed.payload["tools_hash"]["from"],
        json!(before.tools_hash)
    );
    assert_eq!(changed.payload["tools_hash"]["to"], json!(after.tools_hash));
    assert!(changed.payload.get("prompt_version").is_none());
    // The note is in the stored row too, so a later resume still has it.
    let rows = h.store().session_messages(&session, 6, None).unwrap();
    assert_eq!(rows[0].content.len(), 2);
    h.state.close().await;
}

#[tokio::test]
async fn the_cheap_model_titles_a_session_after_its_first_answer() {
    let h = Harness::new(true).await;
    let session = h.three_steps().await;
    let store = h.store();
    assert!(
        h.until(Duration::from_secs(10), || store
            .get_session(&session)
            .unwrap()
            .title_source
            == Some(TitleSource::Generated))
            .await,
        "no generated title"
    );
    let record = store.get_session(&session).unwrap();
    assert_eq!(record.title.as_deref(), Some("Add a hello module"));
    let calls = store
        .list_mentor_calls(&CallFilter::session_of(&session))
        .unwrap();
    assert_eq!(calls.len(), 4);
    let title = calls.iter().find(|c| c.kind == CallKind::Title).unwrap();
    assert_eq!(title.model, TITLE_MODEL);
    assert_eq!(title.status, RunStatus::Ok);
    assert_eq!(title.usage.unwrap().input_tokens, 80);
    assert!(title.cost_micros.unwrap() > 0, "priced like any call");
    let stats = store.stats(&CallFilter::session_of(&session)).unwrap();
    assert_eq!(stats.calls, 4);
    let titled = store
        .session_events(&session)
        .unwrap()
        .into_iter()
        .find(|e| e.summary.kind == kinds::SESSION_TITLE)
        .expect("session.title");
    assert_eq!(titled.payload["title"], "Add a hello module");
    assert_eq!(titled.payload["source"], "generated");
    assert_eq!(titled.payload["call_id"], title.id.as_str());
    let title_request = h
        .requests()
        .await
        .into_iter()
        .find(|r| r["model"] == TITLE_MODEL)
        .unwrap();
    assert_eq!(title_request["max_tokens"], 30);
    assert_eq!(title_request["thinking"]["type"], "disabled");
    let text = title_request["messages"][0]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(
        text.contains("add hello") && text.contains("Hello, world!"),
        "{text}"
    );

    // A second run on the titled session asks for no new title, and a
    // session the user named is never renamed.
    h.script(vec![sse("text")]);
    assert_eq!(h.run(&session, "thanks").await, AgentStatus::Ok);
    let named = h.session(Some("Mine")).await;
    h.script(vec![sse("text")]);
    assert_eq!(h.run(&named, "hi").await, AgentStatus::Ok);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let calls = store.list_mentor_calls(&CallFilter::default()).unwrap();
    assert_eq!(
        calls.iter().filter(|c| c.kind == CallKind::Title).count(),
        1
    );
    let named_record = store.get_session(&named).unwrap();
    assert_eq!(named_record.title.as_deref(), Some("Mine"));
    assert_eq!(named_record.title_source, Some(TitleSource::User));
    h.state.close().await;
}

#[tokio::test]
async fn auto_title_off_leaves_the_prompt_title() {
    let h = Harness::new(false).await;
    let session = h.three_steps().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let record = h.store().get_session(&session).unwrap();
    assert_eq!(record.title_source, Some(TitleSource::Prompt));
    assert_eq!(h.requests().await.len(), 3);
    h.state.close().await;
}
