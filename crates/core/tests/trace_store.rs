//! Trace store integration tests: schema lifecycle, sessions/agents/steps,
//! per-session `seq` under concurrency, blobs, payload spilling, mentor
//! call recording, queries, integrity and the RPC handlers.

use std::sync::Arc;
use std::time::Instant;

use apprentice_api::events::AgentStatus;
use apprentice_api::methods::{
    TraceGet, TraceGetParams, TraceGetResult, TraceList, TraceListParams, TraceListResult,
};
use apprentice_api::server::{Router, RouterConfig};
use apprentice_api::types::{Effort, Usage};
use apprentice_client::{ClientOptions, DaemonClient};
use apprentice_common::paths::Paths;
use apprentice_core::config::TraceConfig;
use apprentice_core::mentor::{
    ContentBlock, MentorError, MentorRequest, MentorResponse, Message, StopReason, SystemBlock,
    Thinking, Timing,
};
use apprentice_core::trace::{
    AgentId, BlobId, CallFilter, CallId, CallKind, EventId, EventQuery, NewAgent, NewEvent,
    NewSession, RunStatus, SCHEMA_VERSION, SessionId, SessionQuery, StepRef, TraceError,
    TraceService, TraceStore, TraceWriter, kinds,
};
use serde_json::json;
use tempfile::TempDir;

struct Home {
    _dir: TempDir,
    paths: Paths,
}

fn home() -> Home {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::from_home(dir.path());
    Home { _dir: dir, paths }
}

fn open(h: &Home) -> TraceStore {
    TraceStore::open(&h.paths).unwrap()
}

fn session(store: &TraceStore) -> SessionId {
    store
        .create_session(&NewSession {
            title: Some("t".into()),
            workspace_path: None,
            workspace_id: None,
            config: json!({"mentor": {"model": "claude-opus-5"}}),
        })
        .unwrap()
}

fn event(session: &SessionId, kind: &str) -> NewEvent {
    NewEvent::new(session.clone(), kind).payload(json!({"n": 1}))
}

fn request() -> MentorRequest {
    MentorRequest {
        model: "claude-opus-5".into(),
        max_tokens: 100,
        system: vec![SystemBlock::new("be brief")],
        messages: vec![Message::user("hi")],
        tools: vec![],
        thinking: Thinking::default(),
        effort: Effort::Low,
        metadata: None,
    }
}

fn response(raw_sse: Option<Vec<u8>>) -> MentorResponse {
    MentorResponse {
        id: "msg_1".into(),
        model: "claude-opus-5".into(),
        content: vec![ContentBlock::text("hello")],
        stop_reason: StopReason::EndTurn,
        stop_details: None,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_input_tokens: 3,
            cache_creation_input_tokens: 0,
        },
        timing: Timing {
            first_byte_ms: 120,
            total_ms: 800,
            attempts: 1,
        },
        request_bytes: 42,
        raw_sse,
    }
}

// ------------------------------------------------------------------ schema

#[test]
fn fresh_open_creates_schema_and_reopen_is_idempotent() {
    let h = home();
    let store = open(&h);
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    assert!(h.paths.data_dir.join("traces.sqlite").is_file());
    drop(store);
    let store = open(&h);
    assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    let usage = store.disk_usage().unwrap();
    assert_eq!(usage.sessions, 0);
    assert!(usage.database_bytes > 0);
}

#[test]
fn newer_schema_refuses_to_open() {
    let h = home();
    drop(open(&h));
    let conn = rusqlite::Connection::open(h.paths.data_dir.join("traces.sqlite")).unwrap();
    conn.execute(
        "UPDATE schema_meta SET value = '99' WHERE key = 'version'",
        [],
    )
    .unwrap();
    drop(conn);
    let err = TraceStore::open(&h.paths).unwrap_err();
    assert!(
        matches!(err, TraceError::SchemaTooNew { found: 99, .. }),
        "{err}"
    );
    assert!(err.to_string().contains("newer than this build"));
}

// --------------------------------------------------------------- lifecycle

#[test]
fn session_agent_step_lifecycle_records_events() {
    let h = home();
    let store = open(&h);
    let sid = session(&store);
    let rec = store.get_session(&sid).unwrap();
    assert_eq!(rec.title.as_deref(), Some("t"));
    assert_eq!(rec.config["mentor"]["model"], "claude-opus-5");

    let aid = store
        .start_agent(&NewAgent::main(sid.clone(), "do the thing"))
        .unwrap();
    let step = store.start_step(&aid).unwrap();
    let step2 = store.start_step(&aid).unwrap();
    assert_eq!(store.get_step(&step).unwrap().seq, 1);
    assert_eq!(store.get_step(&step2).unwrap().seq, 2);
    store.finish_step(&step, RunStatus::Ok).unwrap();
    store
        .finish_agent(&aid, RunStatus::Error, Some(json!({"message": "boom"})))
        .unwrap();

    let agent = store.get_agent(&aid).unwrap();
    assert_eq!(agent.status, RunStatus::Error);
    assert!(agent.ended_at.is_some());
    assert_eq!(store.list_agents(&sid).unwrap().len(), 1);
    assert_eq!(store.list_steps(&aid).unwrap().len(), 2);

    let evs = store.session_events(&sid).unwrap();
    let kinds: Vec<&str> = evs.iter().map(|e| e.summary.kind.as_str()).collect();
    assert_eq!(
        kinds,
        vec![
            kinds::SESSION_CREATED,
            kinds::AGENT_STARTED,
            kinds::AGENT_FINISHED
        ]
    );
    assert_eq!(evs[2].payload["error"]["message"], "boom");
    assert_eq!(evs[2].summary.agent_id.as_deref(), Some(aid.as_str()));
    let seqs: Vec<u64> = evs.iter().map(|e| e.summary.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3]);

    let sessions = store.list_sessions(&SessionQuery::default()).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, sid.as_str());
    assert!(sessions[0].updated_at >= sessions[0].created_at);
    assert_eq!(sessions[0].last_agent_status, Some(AgentStatus::Error));
    assert_eq!(
        store.get_session(&sid).unwrap().last_agent_status,
        Some(RunStatus::Error)
    );

    assert!(matches!(
        store.get_agent(&AgentId::from("nope")),
        Err(TraceError::NotFound { what: "agent", .. })
    ));
    assert!(matches!(
        store.start_step(&AgentId::from("nope")),
        Err(TraceError::NotFound { what: "agent", .. })
    ));
    assert!(matches!(
        store.append(event(&SessionId::from("nope"), "x.y")),
        Err(TraceError::NotFound {
            what: "session",
            ..
        })
    ));
    assert!(matches!(
        store.append(event(&sid, "Bad Kind")),
        Err(TraceError::Invalid { .. })
    ));
}

// ------------------------------------------------------------- concurrency

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_appends_have_gapless_seq_per_session() {
    let h = home();
    let store = Arc::new(open(&h));
    let writer = Arc::new(TraceWriter::spawn(Arc::clone(&store)));
    let a = session(&store);
    let b = session(&store);

    let mut tasks = Vec::new();
    for i in 0..8 {
        let writer = Arc::clone(&writer);
        let sid = if i % 2 == 0 { a.clone() } else { b.clone() };
        tasks.push(tokio::spawn(async move {
            let mut ids = Vec::new();
            for n in 0..50 {
                let ev =
                    NewEvent::new(sid.clone(), "test.tick").payload(json!({"task": i, "n": n}));
                ids.push(writer.append(ev).await.unwrap());
            }
            ids
        }));
    }
    let mut all: Vec<EventId> = Vec::new();
    for t in tasks {
        all.extend(t.await.unwrap());
    }
    assert_eq!(all.len(), 400);
    writer.flush().await.unwrap();

    for sid in [&a, &b] {
        let evs = store.session_events(sid).unwrap();
        // session.created + 200 ticks
        assert_eq!(evs.len(), 201);
        let seqs: Vec<u64> = evs.iter().map(|e| e.summary.seq).collect();
        assert_eq!(seqs, (1..=201).collect::<Vec<_>>());
        assert_eq!(store.event_count(sid).unwrap(), 201);
    }

    writer.shutdown().await;
    assert!(matches!(
        writer.append(event(&a, "test.tick")).await,
        Err(TraceError::WriterClosed)
    ));
}

#[tokio::test]
async fn writer_runs_arbitrary_jobs_in_order() {
    let h = home();
    let store = Arc::new(open(&h));
    let writer = TraceWriter::spawn(Arc::clone(&store));
    let sid = writer
        .run(|s| s.create_session(&NewSession::default()))
        .await
        .unwrap();
    writer.append_detached(event(&sid, "test.a"));
    writer.append_detached(event(&sid, "test.b"));
    let last = writer.append(event(&sid, "test.c")).await.unwrap();
    let evs = store.session_events(&sid).unwrap();
    let kinds: Vec<&str> = evs.iter().map(|e| e.summary.kind.as_str()).collect();
    assert_eq!(kinds, vec!["session.created", "test.a", "test.b", "test.c"]);
    assert_eq!(evs[3].summary.id, last.as_str());
    assert_eq!(writer.queued(), 0);
}

// ------------------------------------------------------------------- blobs

#[test]
fn blob_dedup_yields_one_file_and_refcount_two() {
    let h = home();
    let store = open(&h);
    let id1 = store.put_blob(b"same bytes", "text/plain").unwrap();
    let id2 = store.put_blob(b"same bytes", "text/plain").unwrap();
    assert_eq!(id1, id2);
    let meta = store.blob_meta(&id1).unwrap();
    assert_eq!(meta.refcount, 2);
    assert_eq!(meta.size, 10);
    assert_eq!(meta.media_type, "text/plain");
    let usage = store.disk_usage().unwrap();
    assert_eq!(usage.blob_files, 1);
    assert_eq!(usage.blob_rows, 1);
    assert_eq!(usage.blob_bytes, 10);
    assert_eq!(store.read_blob(&id1).unwrap(), b"same bytes");
    assert_eq!(
        store.blob_path(&id1).parent().unwrap().file_name().unwrap(),
        &id1.as_str()[..2]
    );

    // Referencing an existing blob from an event bumps the refcount too.
    let sid = session(&store);
    store
        .append(NewEvent::new(sid.clone(), "test.ref").blob(id1.clone()))
        .unwrap();
    assert_eq!(store.blob_meta(&id1).unwrap().refcount, 3);
    assert!(matches!(
        store.append(NewEvent::new(sid, "test.ref").blob(BlobId::from("missing"))),
        Err(TraceError::NotFound { what: "blob", .. })
    ));
}

#[test]
fn oversized_payload_strings_move_to_blobs() {
    let h = home();
    let store = TraceStore::open_with(
        &h.paths,
        &TraceConfig {
            capture_raw_sse: false,
            inline_payload_max_bytes: 16,
        },
    )
    .unwrap();
    let sid = session(&store);
    let big = "x".repeat(100);
    let id = store
        .append(NewEvent::new(sid.clone(), "tool.result").payload(json!({
            "summary": "short",
            "output": big,
            "nested": {"more": "y".repeat(50)},
        })))
        .unwrap();
    let ev = store.get_event(&id).unwrap();
    assert_eq!(ev.payload["summary"], "short");
    assert_eq!(ev.payload["output"]["bytes"], 100);
    let blob = BlobId::from(ev.payload["output"]["$blob"].as_str().unwrap());
    assert_eq!(store.read_blob(&blob).unwrap(), big.as_bytes());
    assert_eq!(
        store.blob_meta(&blob).unwrap().media_type,
        "text/plain; charset=utf-8"
    );
    let nested = BlobId::from(ev.payload["nested"]["more"]["$blob"].as_str().unwrap());
    assert_eq!(store.read_blob(&nested).unwrap().len(), 50);
    let mut refs = apprentice_core::trace::blob_refs(&ev.payload);
    refs.sort();
    let mut expected = vec![blob, nested];
    expected.sort();
    assert_eq!(refs, expected);
    assert!(ev.blob_id.is_none());
    assert!(store.integrity_check().unwrap().is_ok());
}

#[test]
fn integrity_check_detects_missing_corrupted_and_bodyless_requests() {
    let h = home();
    let store = open(&h);
    let sid = session(&store);
    let gone = store.put_blob(b"will be deleted", "text/plain").unwrap();
    let bad = store.put_blob(b"will be corrupted", "text/plain").unwrap();
    let fine = store.put_blob(b"fine", "text/plain").unwrap();
    let report = store.integrity_check().unwrap();
    assert!(report.is_ok(), "{report:?}");
    assert_eq!(report.blobs_checked, 3);
    assert_eq!(report.sqlite, "ok");

    std::fs::remove_file(store.blob_path(&gone)).unwrap();
    std::fs::write(store.blob_path(&bad), b"tampered").unwrap();
    let req = store
        .append(NewEvent::new(sid.clone(), kinds::MENTOR_REQUEST).payload(json!({"model": "m"})))
        .unwrap();
    let dangling = store
        .append(
            NewEvent::new(sid, "test.ref")
                .payload(json!({"x": {"$blob": "0000deadbeef", "bytes": 1}})),
        )
        .unwrap();

    let report = store.integrity_check().unwrap();
    assert!(!report.is_ok());
    assert_eq!(report.missing_blobs, vec![gone]);
    assert_eq!(report.corrupted_blobs, vec![bad]);
    assert_eq!(report.requests_without_blob, vec![req]);
    assert_eq!(
        report.dangling_refs,
        vec![(dangling, BlobId::from("0000deadbeef"))]
    );
    assert!(report.missing_blobs.iter().all(|b| *b != fine));
}

#[test]
fn pruned_blob_keeps_row_and_passes_integrity() {
    let h = home();
    let store = open(&h);
    let sid = session(&store);
    let ev = store
        .append(
            NewEvent::new(sid, kinds::APPRENTICE_STATE)
                .payload(json!({"backend": "kv", "bytes": 3}))
                .blob_bytes(vec![1u8, 2, 3], "application/octet-stream"),
        )
        .unwrap();
    let blob = BlobId::from(store.get_event(&ev).unwrap().blob_id.unwrap());
    store.prune_blob(&blob).unwrap();
    let meta = store.blob_meta(&blob).unwrap();
    assert!(meta.pruned_at.is_some());
    assert!(matches!(
        store.read_blob(&blob),
        Err(TraceError::NotFound { what: "blob", .. })
    ));
    let report = store.integrity_check().unwrap();
    assert!(report.is_ok(), "{report:?}");
    assert_eq!(report.blobs_checked, 0);
    // The event still lists the blob and its recorded size.
    let listed = store.list_events(&EventQuery::default()).unwrap();
    assert_eq!(listed.last().unwrap().blob_bytes, Some(3));
}

// ------------------------------------------------------------ mentor calls

#[test]
fn mentor_call_recording_is_replayable_and_aggregated() {
    let h = home();
    let store = open(&h);
    let sid = session(&store);
    let aid = store
        .start_agent(&NewAgent::main(sid.clone(), "task"))
        .unwrap();
    let step = store.start_step(&aid).unwrap();
    let at = StepRef {
        session: sid.clone(),
        agent: aid.clone(),
        step: step.clone(),
    };

    // Call 1: ok, with raw SSE captured.
    let call = CallId::generate();
    let req = request();
    let body = serde_json::to_vec(&req).unwrap();
    let req_ev = store
        .record_mentor_request(
            &at,
            &call,
            CallKind::Step,
            &req,
            &body,
            Some("mentor_system_v1"),
        )
        .unwrap();
    let running = store.get_mentor_call(&call).unwrap();
    assert_eq!(running.status, RunStatus::Running);
    assert_eq!(running.kind, CallKind::Step);
    assert_eq!(running.request_event_id, req_ev);
    assert_eq!(running.effort.as_deref(), Some("low"));
    assert_eq!(running.request_bytes, Some(body.len() as u64));

    let ev = store.get_event(&req_ev).unwrap();
    assert_eq!(ev.payload["message_count"], 1);
    assert_eq!(ev.payload["max_tokens"], 100);
    assert!(ev.payload["system_hash"].is_string());
    assert_eq!(ev.payload["prompt_version"], "mentor_system_v1");
    assert_eq!(
        ev.payload["request_hash"].as_str().unwrap(),
        ev.blob_id.as_deref().unwrap()
    );
    // Replay guarantee: the blob is the exact body.
    let stored = store
        .read_blob(&BlobId::from(ev.blob_id.as_deref().unwrap()))
        .unwrap();
    assert_eq!(stored, body);

    let resp_ev = store
        .record_mentor_response(
            &at,
            &call,
            &response(Some(b"event: ping\n\n".to_vec())),
            Some(1234),
        )
        .unwrap();
    let done = store.get_mentor_call(&call).unwrap();
    assert_eq!(done.status, RunStatus::Ok);
    assert_eq!(done.response_event_id, Some(resp_ev.clone()));
    assert_eq!(done.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(done.http_status, Some(200));
    assert_eq!(done.usage.unwrap().input_tokens, 10);
    assert_eq!(done.cost_micros, Some(1234));
    assert_eq!(done.first_byte_ms, Some(120));
    assert!(done.ended_at.is_some());

    let ev = store.get_event(&resp_ev).unwrap();
    assert_eq!(ev.payload["usage"]["output_tokens"], 5);
    let content: Vec<ContentBlock> = serde_json::from_slice(
        &store
            .read_blob(&BlobId::from(ev.blob_id.as_deref().unwrap()))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(content, vec![ContentBlock::text("hello")]);
    let raw = BlobId::from(ev.payload["raw_sse_blob_id"]["$blob"].as_str().unwrap());
    assert_eq!(store.read_blob(&raw).unwrap(), b"event: ping\n\n");

    // Call 2: a retried attempt, then a final error; unpriced.
    let call2 = CallId::generate();
    store
        .record_mentor_request(&at, &call2, CallKind::Step, &req, &body, None)
        .unwrap();
    store
        .record_mentor_error(&at, &call2, &MentorError::Overloaded, 0, false)
        .unwrap();
    assert_eq!(
        store.get_mentor_call(&call2).unwrap().status,
        RunStatus::Running
    );
    store
        .record_mentor_error(
            &at,
            &call2,
            &MentorError::Api {
                status: 500,
                kind: "api_error".into(),
                message: "nope".into(),
            },
            1,
            true,
        )
        .unwrap();
    let failed = store.get_mentor_call(&call2).unwrap();
    assert_eq!(failed.status, RunStatus::Error);
    assert_eq!(failed.http_status, Some(500));
    assert!(failed.response_event_id.is_none());
    assert!(failed.usage.is_none());

    // Call 3: cancelled.
    let call3 = CallId::generate();
    store
        .record_mentor_request(&at, &call3, CallKind::Step, &req, &body, None)
        .unwrap();
    store
        .record_mentor_error(&at, &call3, &MentorError::Cancelled, 0, true)
        .unwrap();
    assert_eq!(
        store.get_mentor_call(&call3).unwrap().status,
        RunStatus::Cancelled
    );

    // Call 4: ok but unpriced.
    let call4 = CallId::generate();
    store
        .record_mentor_request(&at, &call4, CallKind::Title, &req, &body, None)
        .unwrap();
    store
        .record_mentor_response(&at, &call4, &response(None), None)
        .unwrap();

    let rows = store.list_mentor_calls(&CallFilter::default()).unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(
        store
            .list_mentor_calls(&CallFilter {
                status: Some(RunStatus::Ok),
                ..CallFilter::default()
            })
            .unwrap()
            .len(),
        2
    );

    let totals = store.stats(&CallFilter::session_of(&sid)).unwrap();
    assert_eq!(totals.calls, 4);
    assert_eq!(totals.input_tokens, 20);
    assert_eq!(totals.output_tokens, 10);
    assert_eq!(totals.cache_read_tokens, 6);
    assert_eq!(totals.cost_micros, 1234);
    assert_eq!(totals.unpriced_calls, 1);
    let none = store
        .stats(&CallFilter {
            model: Some("other".into()),
            ..CallFilter::default()
        })
        .unwrap();
    assert_eq!(none.calls, 0);

    // The request blob is shared by all four calls (same body).
    let blob = BlobId::from(store.get_event(&req_ev).unwrap().blob_id.unwrap());
    assert_eq!(store.blob_meta(&blob).unwrap().refcount, 4);
    assert!(store.integrity_check().unwrap().is_ok());
}

// ----------------------------------------------------------------- queries

#[test]
fn list_events_filters_and_pages_backwards() {
    let h = home();
    let store = open(&h);
    let sid = session(&store);
    let other = session(&store);
    let aid = store
        .start_agent(&NewAgent::main(sid.clone(), "t"))
        .unwrap();
    for n in 0..10 {
        let kind = if n % 2 == 0 { "test.even" } else { "test.odd" };
        let mut ev = event(&sid, kind);
        if n < 5 {
            ev = ev.agent(aid.clone());
        }
        store.append(ev).unwrap();
    }
    store.append(event(&other, "test.even")).unwrap();

    let all = store
        .list_events(&EventQuery::session(sid.clone()))
        .unwrap();
    assert_eq!(all.len(), 12); // created + started + 10
    assert!(all.windows(2).all(|w| w[0].seq < w[1].seq));

    let odd = store
        .list_events(&EventQuery {
            kinds: vec!["test.odd".into()],
            ..EventQuery::session(sid.clone())
        })
        .unwrap();
    assert_eq!(odd.len(), 5);

    let by_agent = store
        .list_events(&EventQuery {
            agent_id: Some(aid.clone()),
            ..EventQuery::default()
        })
        .unwrap();
    assert_eq!(by_agent.len(), 6); // agent.started + 5

    let last3 = store
        .list_events(&EventQuery {
            limit: Some(3),
            before_seq: Some(u64::MAX),
            ..EventQuery::session(sid.clone())
        })
        .unwrap();
    let seqs: Vec<u64> = last3.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![10, 11, 12]);
    let prev = store
        .list_events(&EventQuery {
            limit: Some(3),
            before_seq: Some(10),
            ..EventQuery::session(sid.clone())
        })
        .unwrap();
    let seqs: Vec<u64> = prev.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![7, 8, 9]);
    let after = store
        .list_events(&EventQuery {
            after_seq: Some(10),
            ..EventQuery::session(sid)
        })
        .unwrap();
    assert_eq!(after.len(), 2);

    let everything = store.list_events(&EventQuery::default()).unwrap();
    assert_eq!(everything.len(), 14);
}

#[tokio::test]
async fn rpc_handlers_over_a_router() {
    let h = home();
    let store = Arc::new(open(&h));
    let sid = session(&store);
    let ev = store
        .append(
            NewEvent::new(sid.clone(), kinds::USER_MESSAGE)
                .payload(json!({"text_len": 5}))
                .blob_bytes("hello", "text/plain"),
        )
        .unwrap();

    let svc = Arc::new(TraceService::new(Arc::clone(&store)));
    let mut router = Router::new(RouterConfig {
        daemon_version: "0".into(),
        pid: 1,
        token: None,
    });
    svc.register(&mut router);
    assert_eq!(router.methods(), vec!["trace.get", "trace.list"]);

    let (server_side, client_side) = tokio::io::duplex(1 << 16);
    let (sr, sw) = tokio::io::split(server_side);
    let router = Arc::new(router);
    tokio::spawn(async move {
        let _ = router.serve(sr, sw).await;
    });
    let (cr, cw) = tokio::io::split(client_side);
    let client = DaemonClient::from_streams(cr, cw, ClientOptions::default());
    client.hello("test", "0", None).await.unwrap();

    let l: TraceListResult = client
        .call::<TraceList>(TraceListParams {
            session_id: Some(sid.as_str().to_owned()),
            kinds: vec![kinds::USER_MESSAGE.into()],
            ..TraceListParams::default()
        })
        .await
        .unwrap();
    assert_eq!(l.events.len(), 1);
    assert_eq!(l.events[0].id, ev.as_str());
    assert_eq!(l.events[0].blob_bytes, Some(5));

    let g: TraceGetResult = client
        .call::<TraceGet>(TraceGetParams {
            event_id: ev.as_str().to_owned(),
            include_blob: true,
        })
        .await
        .unwrap();
    assert_eq!(g.event.payload["text_len"], 5);
    assert_eq!(g.blob.as_deref(), Some("hello"));

    let err = client
        .call::<TraceGet>(TraceGetParams {
            event_id: "nope".into(),
            include_blob: false,
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
}

// -------------------------------------------------------------- throughput

/// Informational: one transaction per event is commit-bound (about 200 us
/// per append on Windows/NTFS). Prints the timing; the acceptance number
/// is measured on the writer path below.
#[test]
#[ignore = "timing-sensitive; run with --ignored on the reference machine"]
fn ten_thousand_direct_appends_timing() {
    let h = home();
    let store = open(&h);
    let sid = session(&store);
    let start = Instant::now();
    for n in 0..10_000 {
        store
            .append(NewEvent::new(sid.clone(), "bench.tick").payload(json!({"n": n})))
            .unwrap();
    }
    let elapsed = start.elapsed();
    eprintln!("10k direct appends: {elapsed:?}");
    assert_eq!(store.event_count(&sid).unwrap(), 10_001);
}

/// Acceptance: 10k small events append in < 2 s on the reference machine
/// (through the writer, where queued appends share a transaction).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "timing-sensitive; run with --ignored on the reference machine"]
async fn ten_thousand_writer_appends_are_batched() {
    let h = home();
    let store = Arc::new(open(&h));
    let writer = Arc::new(TraceWriter::spawn(Arc::clone(&store)));
    let sid = session(&store);
    let start = Instant::now();
    let mut tasks = Vec::new();
    for t in 0..8 {
        let writer = Arc::clone(&writer);
        let sid = sid.clone();
        tasks.push(tokio::spawn(async move {
            for n in 0..1250 {
                writer
                    .append(
                        NewEvent::new(sid.clone(), "bench.tick").payload(json!({"t": t, "n": n})),
                    )
                    .await
                    .unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let elapsed = start.elapsed();
    eprintln!("10k writer appends (8 tasks): {elapsed:?}");
    assert_eq!(store.event_count(&sid).unwrap(), 10_001);
    assert!(elapsed.as_secs_f64() < 2.0, "took {elapsed:?}");
}
