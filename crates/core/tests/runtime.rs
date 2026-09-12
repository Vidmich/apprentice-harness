//! The agent runtime (task M00-11) against `AppState` with the mentor
//! pointed at a wiremock server: the recorded trace of a round trip,
//! cancellation, errors, truncation, shutdown, and the `agent.*` RPC
//! methods over an in-memory router.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use apprentice_api::events::{AgentStatus, Event, EventNotification};
use apprentice_api::jsonrpc::codes;
use apprentice_api::methods::{
    AgentCancel, AgentIdParams, AgentRun, AgentRunParams, AgentSubscribe, SessionCreateParams,
};
use apprentice_api::server::{Router, RouterConfig};
use apprentice_api::types::{RunOptions, TraceEvent, Usage};
use apprentice_client::{ClientError, ClientOptions, DaemonClient};
use apprentice_core::app::AppState;
use apprentice_core::config::{ConfigLoader, Paths};
use apprentice_core::runtime::{AgentHandle, run_agent};
use apprentice_core::secrets::{Secret, SecretStore as _, api_key_name};
use apprentice_core::trace::{
    AgentId, BlobId, CallFilter, RunStatus, SessionId, TraceStore, kinds, sha256_hex,
};
use serde_json::Value;
use tokio::sync::broadcast;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sse")
        .join(format!("{name}.txt"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn sse(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(fixture(name), "text/event-stream")
}

async fn mount(server: &MockServer, template: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(template)
        .mount(server)
        .await;
}

struct Harness {
    _home: tempfile::TempDir,
    state: Arc<AppState>,
    server: MockServer,
}

impl Harness {
    /// A state whose mentor talks to a fresh wiremock server, with a key
    /// in the file secret store.
    async fn new() -> Self {
        let server = MockServer::start().await;
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(home.path());
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        std::fs::write(
            paths.config_file(),
            format!(
                "[daemon]\nsecret_store = \"file\"\n\n[mentor]\nbase_url = \"{}\"\nmax_retries = 0\ntimeout_s = 30\n",
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
        Self {
            _home: home,
            state,
            server,
        }
    }

    async fn session(&self) -> SessionId {
        self.state
            .session_create(&SessionCreateParams {
                workspace: None,
                title: Some("runtime test".into()),
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
    ) -> (AgentHandle, broadcast::Receiver<Event>) {
        run_agent(
            &self.state,
            session.clone(),
            prompt.into(),
            RunOptions::default(),
        )
        .await
        .unwrap()
    }
}

/// Collects events until the terminal one.
async fn collect(rx: &mut broadcast::Receiver<Event>) -> Vec<Event> {
    let mut out = Vec::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
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

fn kinds_of(events: &[TraceEvent]) -> Vec<&str> {
    events.iter().map(|e| e.summary.kind.as_str()).collect()
}

fn find<'a>(events: &'a [TraceEvent], kind: &str) -> &'a TraceEvent {
    events
        .iter()
        .find(|e| e.summary.kind == kind)
        .unwrap_or_else(|| panic!("no {kind} in {:?}", kinds_of(events)))
}

fn blob(store: &TraceStore, ev: &TraceEvent) -> Vec<u8> {
    let id = ev
        .blob_id
        .as_deref()
        .unwrap_or_else(|| panic!("{} has no blob", ev.summary.kind));
    store.read_blob(&BlobId::from(id)).unwrap()
}

fn finished(
    events: &[Event],
) -> (
    AgentStatus,
    Option<&apprentice_api::jsonrpc::RpcError>,
    bool,
) {
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

#[tokio::test]
async fn a_round_trip_streams_text_and_records_the_exchange() {
    let h = Harness::new().await;
    mount(&h.server, sse("text")).await;
    let session = h.session().await;
    let (handle, mut rx) = h.run(&session, "hello").await;
    assert!(handle.is_running());
    assert_eq!(h.state.agents().running(), 1);

    let events = collect(&mut rx).await;
    let agent = handle.agent_id.to_string();
    assert!(
        matches!(&events[0], Event::AgentStarted { agent_id, session_id }
        if *agent_id == agent && *session_id == session.to_string())
    );
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            Event::AgentTextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello, world!");
    let usage = events
        .iter()
        .find_map(|e| match e {
            Event::AgentUsage {
                usage, cost_usd, ..
            } => Some((*usage, *cost_usd)),
            _ => None,
        })
        .expect("agent.usage");
    assert_eq!(
        usage.0,
        Usage {
            input_tokens: 25,
            output_tokens: 12,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        }
    );
    // Opus 5 list price: 25 × $5/M + 12 × $25/M.
    assert_eq!(usage.1, Some(0.000_425));
    assert_eq!(finished(&events), (AgentStatus::Ok, None, false));
    assert_eq!(handle.status(), Some(AgentStatus::Ok));
    assert_eq!(h.state.agents().running(), 0);
    assert!(h.state.agents().last_finished().is_some());

    // The trace, in order.
    let store = h.store();
    let trace = store.session_events(&session).unwrap();
    assert_eq!(
        kinds_of(&trace),
        [
            kinds::SESSION_CREATED,
            kinds::AGENT_STARTED,
            kinds::USER_MESSAGE,
            kinds::MENTOR_REQUEST,
            kinds::MENTOR_RESPONSE,
            kinds::ASSISTANT_MESSAGE,
            kinds::AGENT_FINISHED,
        ]
    );
    let seqs: Vec<u64> = trace.iter().map(|e| e.summary.seq).collect();
    assert_eq!(seqs, [1, 2, 3, 4, 5, 6, 7]);
    let started = find(&trace, kinds::AGENT_STARTED);
    assert_eq!(started.payload["task_text"], "hello");
    assert_eq!(started.summary.agent_id.as_deref(), Some(agent.as_str()));
    let user = find(&trace, kinds::USER_MESSAGE);
    assert_eq!(user.payload["text_len"], 5);
    assert_eq!(blob(store, user), b"hello");

    // The request blob is byte for byte what the server received.
    let received = h.server.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let request = find(&trace, kinds::MENTOR_REQUEST);
    let body = blob(store, request);
    assert_eq!(body, received[0].body);
    assert_eq!(request.payload["request_hash"], sha256_hex(&body));
    assert_eq!(request.payload["bytes"], body.len());
    assert_eq!(request.payload["model"], "claude-opus-5");
    assert_eq!(request.payload["message_count"], 1);
    let wire: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(wire["stream"], true);
    assert_eq!(wire["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(wire["messages"][0]["content"][0]["text"], "hello");
    assert_eq!(
        wire["messages"][0]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(wire["thinking"]["type"], "adaptive");
    assert_eq!(wire["output_config"]["effort"], "high");
    assert!(wire.get("tools").is_none());

    let response = find(&trace, kinds::MENTOR_RESPONSE);
    assert_eq!(response.payload["usage"]["input_tokens"], 25);
    assert_eq!(response.payload["usage"]["output_tokens"], 12);
    assert_eq!(response.payload["stop_reason"], "end_turn");
    assert_eq!(response.payload["attempts"], 1);
    let content: Value = serde_json::from_slice(&blob(store, response)).unwrap();
    assert_eq!(content[0]["text"], "Hello, world!");
    let call_id = response.payload["call_id"].as_str().unwrap();
    assert_eq!(request.payload["call_id"], call_id);

    let message = find(&trace, kinds::ASSISTANT_MESSAGE);
    assert_eq!(blob(store, message), b"Hello, world!");
    assert_eq!(message.payload["text_len"], 13);
    assert_eq!(message.payload["stop_reason"], "end_turn");
    assert_eq!(message.payload["truncated"], false);
    assert_eq!(message.payload["call_id"], call_id);
    assert_eq!(message.summary.step_id, request.summary.step_id);

    let done = find(&trace, kinds::AGENT_FINISHED);
    assert_eq!(done.payload["status"], "ok");
    assert!(done.payload.get("error").is_none());

    // Rows: the agent, its one step, the priced call, and the totals.
    let agent_id = AgentId::from(agent.as_str());
    let record = store.get_agent(&agent_id).unwrap();
    assert_eq!(record.status, RunStatus::Ok);
    assert!(record.ended_at.is_some());
    let steps = store.list_steps(&agent_id).unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].status, RunStatus::Ok);
    let calls = store
        .list_mentor_calls(&CallFilter::session_of(&session))
        .unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id.as_str(), call_id);
    assert_eq!(calls[0].status, RunStatus::Ok);
    assert_eq!(calls[0].cost_micros, Some(425));
    assert_eq!(calls[0].request_bytes, Some(body.len() as u64));
    assert_eq!(calls[0].usage.unwrap().output_tokens, 12);
    let totals = store.stats(&CallFilter::session_of(&session)).unwrap();
    assert_eq!(totals.calls, 1);
    assert_eq!(totals.cost_micros, 425);
    assert_eq!(totals.input_tokens, 25);
    h.state.close().await;
}

#[tokio::test]
async fn cancellation_ends_the_call_and_is_recorded() {
    let h = Harness::new().await;
    mount(&h.server, sse("text").set_delay(Duration::from_secs(30))).await;
    let session = h.session().await;
    let (handle, mut rx) = h.run(&session, "slow").await;
    // Cancel once the call is on the wire.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = std::time::Instant::now();
    handle.cancel.cancel();
    let events = collect(&mut rx).await;
    assert!(started.elapsed() < Duration::from_secs(5));
    let (status, error, _) = finished(&events);
    assert_eq!(status, AgentStatus::Cancelled);
    assert_eq!(error.unwrap().kind(), Some("cancelled"));
    assert!(!events.iter().any(|e| matches!(e, Event::AgentUsage { .. })));

    let store = h.store();
    let trace = store.session_events(&session).unwrap();
    assert_eq!(
        kinds_of(&trace),
        [
            kinds::SESSION_CREATED,
            kinds::AGENT_STARTED,
            kinds::USER_MESSAGE,
            kinds::MENTOR_REQUEST,
            kinds::MENTOR_ERROR,
            kinds::AGENT_FINISHED,
        ]
    );
    let err = find(&trace, kinds::MENTOR_ERROR);
    assert_eq!(err.payload["kind"], "cancelled");
    let done = find(&trace, kinds::AGENT_FINISHED);
    assert_eq!(done.payload["status"], "cancelled");
    assert_eq!(done.payload["error"]["data"]["kind"], "cancelled");
    let calls = store
        .list_mentor_calls(&CallFilter::session_of(&session))
        .unwrap();
    assert_eq!(calls[0].status, RunStatus::Cancelled);
    assert_eq!(calls[0].cost_micros, None);
    let steps = store.list_steps(&handle.agent_id).unwrap();
    assert_eq!(steps[0].status, RunStatus::Cancelled);
    assert_eq!(
        store.get_agent(&handle.agent_id).unwrap().status,
        RunStatus::Cancelled
    );
    h.state.close().await;
}

#[tokio::test]
async fn an_api_error_finishes_the_agent_with_a_mentor_error() {
    let h = Harness::new().await;
    mount(
        &h.server,
        ResponseTemplate::new(401).set_body_string(
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
        ),
    )
    .await;
    let session = h.session().await;
    let (handle, mut rx) = h.run(&session, "hello").await;
    let events = collect(&mut rx).await;
    let (status, error, _) = finished(&events);
    assert_eq!(status, AgentStatus::Error);
    let error = error.unwrap();
    assert_eq!(error.code, codes::MENTOR_ERROR);
    assert_eq!(error.kind(), Some("mentor_error"));
    assert!(
        error.message.contains("authentication"),
        "{}",
        error.message
    );
    let details = error.data.as_ref().unwrap().details.as_ref().unwrap();
    assert_eq!(details["http_status"], 401);
    assert_eq!(details["reason"], "auth");

    let store = h.store();
    let trace = store.session_events(&session).unwrap();
    let err = find(&trace, kinds::MENTOR_ERROR);
    assert_eq!(err.payload["kind"], "auth");
    assert_eq!(err.payload["http_status"], 401);
    let done = find(&trace, kinds::AGENT_FINISHED);
    assert_eq!(done.payload["status"], "error");
    assert_eq!(done.payload["error"]["data"]["kind"], "mentor_error");
    let calls = store
        .list_mentor_calls(&CallFilter::session_of(&session))
        .unwrap();
    assert_eq!(calls[0].status, RunStatus::Error);
    assert_eq!(calls[0].http_status, Some(401));
    assert_eq!(
        store.get_agent(&handle.agent_id).unwrap().status,
        RunStatus::Error
    );
    h.state.close().await;
}

#[tokio::test]
async fn a_missing_key_fails_before_any_call() {
    if std::env::var_os("ANTHROPIC_API_KEY").is_some_and(|v| !v.is_empty()) {
        eprintln!("skipped: ANTHROPIC_API_KEY is set in this environment");
        return;
    }
    let h = Harness::new().await;
    h.state
        .secrets()
        .delete(&api_key_name("anthropic"))
        .unwrap();
    h.state.invalidate_mentor();
    let session = h.session().await;
    let (_handle, mut rx) = h.run(&session, "hello").await;
    let events = collect(&mut rx).await;
    let (status, error, _) = finished(&events);
    assert_eq!(status, AgentStatus::Error);
    let error = error.unwrap();
    assert!(error.message.contains("no API key"), "{}", error.message);
    let trace = h.store().session_events(&session).unwrap();
    assert_eq!(
        kinds_of(&trace),
        [
            kinds::SESSION_CREATED,
            kinds::AGENT_STARTED,
            kinds::USER_MESSAGE,
            kinds::AGENT_FINISHED,
        ]
    );
    assert!(h.server.received_requests().await.unwrap().is_empty());
    h.state.close().await;
}

#[tokio::test]
async fn max_tokens_finishes_ok_but_truncated() {
    let h = Harness::new().await;
    mount(&h.server, sse("max_tokens")).await;
    let session = h.session().await;
    let (_handle, mut rx) = h.run(&session, "go on").await;
    let events = collect(&mut rx).await;
    assert_eq!(finished(&events), (AgentStatus::Ok, None, true));
    let trace = h.store().session_events(&session).unwrap();
    let message = find(&trace, kinds::ASSISTANT_MESSAGE);
    assert_eq!(message.payload["truncated"], true);
    assert_eq!(message.payload["stop_reason"], "max_tokens");
    h.state.close().await;
}

#[tokio::test]
async fn an_unknown_session_is_rejected_before_anything_is_recorded() {
    let h = Harness::new().await;
    let err = run_agent(
        &h.state,
        SessionId::from("nope"),
        "hello".into(),
        RunOptions::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.kind(), Some("not_found"), "{err:?}");
    assert_eq!(h.state.agents().running(), 0);
    h.state.close().await;
}

#[tokio::test]
async fn shutdown_cancels_in_flight_agents_and_records_their_end() {
    let h = Harness::new().await;
    mount(&h.server, sse("text").set_delay(Duration::from_secs(30))).await;
    let session = h.session().await;
    let (handle, mut rx) = h.run(&session, "slow").await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let started = std::time::Instant::now();
    h.state.close().await;
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(h.state.shutdown().is_cancelled());
    assert!(handle.cancel.is_cancelled());
    assert_eq!(h.state.agents().running(), 0);
    let events = collect(&mut rx).await;
    assert_eq!(finished(&events).0, AgentStatus::Cancelled);

    // Everything was written and flushed before the writer stopped.
    let trace = h.store().session_events(&session).unwrap();
    assert_eq!(
        kinds_of(&trace).last().copied(),
        Some(kinds::AGENT_FINISHED)
    );
    assert_eq!(
        find(&trace, kinds::AGENT_FINISHED).payload["status"],
        "cancelled"
    );
    assert_eq!(
        find(&trace, kinds::MENTOR_ERROR).payload["kind"],
        "cancelled"
    );
    assert_eq!(
        h.store().get_agent(&handle.agent_id).unwrap().status,
        RunStatus::Cancelled
    );
}

// ------------------------------------------------------------ over RPC

/// A client talking to the state's router over an in-memory duplex.
async fn rpc_client(state: &Arc<AppState>) -> (DaemonClient, tokio::task::JoinHandle<()>) {
    let mut router = Router::new(RouterConfig {
        daemon_version: "0".into(),
        pid: 1,
        token: None,
    });
    state.register(&mut router);
    let router = Arc::new(router);
    let (server_side, client_side) = tokio::io::duplex(1 << 20);
    let (sr, sw) = tokio::io::split(server_side);
    let server = tokio::spawn(async move {
        let _ = router.serve(sr, sw).await;
    });
    let (cr, cw) = tokio::io::split(client_side);
    let client = DaemonClient::from_streams(cr, cw, ClientOptions::default());
    client.hello("runtime-test", "0", None).await.unwrap();
    (client, server)
}

async fn drain(stream: &mut apprentice_client::EventStream) -> Vec<EventNotification> {
    let mut out = Vec::new();
    while let Some(n) = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .expect("stream ends in time")
    {
        let done = n.event.is_terminal();
        out.push(n);
        if done {
            break;
        }
    }
    out
}

#[tokio::test]
async fn two_concurrent_runs_keep_their_events_and_traces_apart() {
    let h = Harness::new().await;
    mount(&h.server, sse("text").set_delay(Duration::from_millis(300))).await;
    let (client, server) = rpc_client(&h.state).await;
    let a = h.session().await;
    let b = h.session().await;
    let run = |session: &SessionId, prompt: &str| {
        client.call_streaming::<AgentRun>(AgentRunParams {
            session_id: session.to_string(),
            prompt: prompt.into(),
            options: RunOptions::default(),
        })
    };
    let (ra, mut sa) = run(&a, "first").await.unwrap();
    let (rb, mut sb) = run(&b, "second").await.unwrap();
    assert_ne!(ra.agent_id, rb.agent_id);
    assert_eq!(ra.subscription, ra.agent_id);
    assert_eq!(h.state.agents().running(), 2);

    let (ea, eb) = tokio::join!(drain(&mut sa), drain(&mut sb));
    for (events, result) in [(&ea, &ra), (&eb, &rb)] {
        assert!(events.iter().all(|n| n.subscription == result.subscription));
        let seqs: Vec<u64> = events.iter().map(|n| n.seq).collect();
        assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
        assert!(matches!(events[0].event, Event::AgentStarted { .. }));
        assert!(matches!(
            events.last().unwrap().event,
            Event::AgentFinished {
                status: AgentStatus::Ok,
                ..
            }
        ));
    }
    for (session, result, prompt) in [(&a, &ra, "first"), (&b, &rb, "second")] {
        let trace = h.store().session_events(session).unwrap();
        assert_eq!(trace.len(), 7, "{:?}", kinds_of(&trace));
        let seqs: Vec<u64> = trace.iter().map(|e| e.summary.seq).collect();
        assert_eq!(seqs, [1, 2, 3, 4, 5, 6, 7]);
        assert!(
            trace[1..]
                .iter()
                .all(|e| e.summary.agent_id.as_deref() == Some(result.agent_id.as_str()))
        );
        assert_eq!(
            find(&trace, kinds::AGENT_STARTED).payload["task_text"],
            prompt
        );
    }
    assert_eq!(h.server.received_requests().await.unwrap().len(), 2);
    server.abort();
    h.state.close().await;
}

#[tokio::test]
async fn cancel_and_subscribe_over_rpc() {
    let h = Harness::new().await;
    mount(&h.server, sse("text").set_delay(Duration::from_secs(30))).await;
    let (client, server) = rpc_client(&h.state).await;
    let (other, other_server) = rpc_client(&h.state).await;
    let session = h.session().await;
    let (run, mut stream) = client
        .call_streaming::<AgentRun>(AgentRunParams {
            session_id: session.to_string(),
            prompt: "slow".into(),
            options: RunOptions::default(),
        })
        .await
        .unwrap();

    // A second connection attaches while the agent runs.
    let (sub, mut attached) = other
        .call_streaming::<AgentSubscribe>(AgentIdParams {
            agent_id: run.agent_id.clone(),
        })
        .await
        .unwrap();
    assert!(sub.running);
    assert_eq!(sub.subscription, run.agent_id);

    // Unknown agents cannot be cancelled.
    let err = client
        .call::<AgentCancel>(AgentIdParams {
            agent_id: "nope".into(),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.kind() == Some("not_found")),
        "{err:?}"
    );

    tokio::time::sleep(Duration::from_millis(200)).await;
    client
        .call::<AgentCancel>(AgentIdParams {
            agent_id: run.agent_id.clone(),
        })
        .await
        .unwrap();
    let events = drain(&mut stream).await;
    assert!(matches!(
        events.last().unwrap().event,
        Event::AgentFinished {
            status: AgentStatus::Cancelled,
            ..
        }
    ));
    // The late subscriber saw the end too (and nothing before its start).
    let seen = drain(&mut attached).await;
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(seen[0].event.is_terminal());

    // Cancelling again is harmless; subscribing after the end replays
    // only the terminal event, from the trace.
    client
        .call::<AgentCancel>(AgentIdParams {
            agent_id: run.agent_id.clone(),
        })
        .await
        .unwrap();
    let (sub, mut late) = other
        .call_streaming::<AgentSubscribe>(AgentIdParams {
            agent_id: run.agent_id.clone(),
        })
        .await
        .unwrap();
    assert!(!sub.running);
    let seen = drain(&mut late).await;
    assert_eq!(seen.len(), 1);
    assert!(matches!(
        seen[0].event,
        Event::AgentFinished {
            status: AgentStatus::Cancelled,
            ..
        }
    ));
    let err = other
        .call_streaming::<AgentSubscribe>(AgentIdParams {
            agent_id: "nope".into(),
        })
        .await
        .unwrap_err();
    assert!(
        matches!(&err, ClientError::Rpc(e) if e.kind() == Some("not_found")),
        "{err:?}"
    );

    server.abort();
    other_server.abort();
    h.state.close().await;
}

#[tokio::test]
async fn a_dropped_connection_does_not_cancel_the_agent() {
    let h = Harness::new().await;
    mount(&h.server, sse("text").set_delay(Duration::from_millis(500))).await;
    let (client, server) = rpc_client(&h.state).await;
    let session = h.session().await;
    let (run, _stream) = client
        .call_streaming::<AgentRun>(AgentRunParams {
            session_id: session.to_string(),
            prompt: "hello".into(),
            options: RunOptions::default(),
        })
        .await
        .unwrap();
    let handle = h
        .state
        .agents()
        .get(&AgentId::from(run.agent_id.as_str()))
        .unwrap();
    drop(client);
    server.abort();

    let event = tokio::time::timeout(Duration::from_secs(10), handle.finished())
        .await
        .unwrap();
    assert!(matches!(
        event,
        Event::AgentFinished {
            status: AgentStatus::Ok,
            ..
        }
    ));
    assert_eq!(
        h.store().get_agent(&handle.agent_id).unwrap().status,
        RunStatus::Ok
    );
    h.state.close().await;
}
