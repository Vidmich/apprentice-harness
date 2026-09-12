//! Router behaviour exercised with a raw line-oriented client over an
//! in-memory duplex stream: handshake enforcement, error codes, framing
//! limits, panic isolation and streaming notifications.

use std::sync::Arc;
use std::time::Duration;

use apprentice_api::API_VERSION;
use apprentice_api::codec::MAX_LINE_BYTES;
use apprentice_api::events::{AgentStatus, Event};
use apprentice_api::jsonrpc::{RpcError, codes};
use apprentice_api::methods::{
    AgentIdParams, AgentRun, AgentRunParams, AgentRunResult, DaemonStatus, DaemonStatusResult,
    Empty, Method,
};
use apprentice_api::server::{Connection, Router, RouterConfig};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

struct RawClient {
    reader: BufReader<ReadHalf<DuplexStream>>,
    writer: WriteHalf<DuplexStream>,
}

impl RawClient {
    async fn send(&mut self, line: &str) {
        self.writer.write_all(line.as_bytes()).await.unwrap();
        self.writer.write_all(b"\n").await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        let mut line = String::new();
        let timeout =
            tokio::time::timeout(Duration::from_secs(5), self.reader.read_line(&mut line));
        let n = timeout
            .await
            .expect("timed out waiting for a line")
            .unwrap();
        assert!(n > 0, "connection closed");
        serde_json::from_str(&line).unwrap()
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string())
            .await;
        self.recv().await
    }

    async fn hello(&mut self, token: Option<&str>) -> Value {
        self.request(
            0,
            "daemon.hello",
            json!({"client":"test","client_version":"0","api_version":API_VERSION,"token":token}),
        )
        .await
    }
}

fn router(token: Option<&str>) -> Router {
    let mut r = Router::new(RouterConfig {
        daemon_version: "9.9.9".into(),
        pid: 42,
        token: token.map(str::to_owned),
    });
    r.add::<DaemonStatus, _, _>(|_conn, Empty {}| async {
        Ok(DaemonStatusResult {
            version: "9.9.9".into(),
            pid: 42,
            uptime_s: 1,
            sessions_open: 0,
            data_dir: "/tmp".into(),
            log_file: None,
        })
    });
    r
}

fn start(router: Router) -> RawClient {
    let (server_side, client_side) = tokio::io::duplex(1 << 20);
    let (sr, sw) = tokio::io::split(server_side);
    let router = Arc::new(router);
    tokio::spawn(async move {
        let _ = router.serve(sr, sw).await;
    });
    let (cr, cw) = tokio::io::split(client_side);
    RawClient {
        reader: BufReader::new(cr),
        writer: cw,
    }
}

#[tokio::test]
async fn methods_before_hello_are_unauthorized() {
    let mut c = start(router(None));
    let resp = c.request(1, "daemon.status", json!({})).await;
    assert_eq!(resp["error"]["code"], codes::UNAUTHORIZED);
    assert_eq!(resp["error"]["data"]["kind"], "unauthorized");
    assert_eq!(resp["id"], 1);
}

#[tokio::test]
async fn hello_checks_token_and_api_version() {
    let mut c = start(router(Some("secret")));
    let bad = c.hello(Some("wrong")).await;
    assert_eq!(bad["error"]["code"], codes::UNAUTHORIZED);

    let mismatch = c
        .request(
            0,
            "daemon.hello",
            json!({"client":"t","client_version":"0","api_version":API_VERSION + 1,"token":"secret"}),
        )
        .await;
    assert_eq!(mismatch["error"]["code"], codes::INCOMPATIBLE_API);
    assert_eq!(
        mismatch["error"]["data"]["details"]["daemon_api"],
        API_VERSION
    );

    let ok = c.hello(Some("secret")).await;
    assert_eq!(ok["result"]["daemon_version"], "9.9.9");
    assert_eq!(ok["result"]["api_version"], API_VERSION);
    assert_eq!(ok["result"]["pid"], 42);

    let status = c.request(2, "daemon.status", json!(null)).await;
    assert_eq!(status["result"]["version"], "9.9.9");
}

#[tokio::test]
async fn unknown_method_and_bad_params() {
    let mut c = start(router(None));
    c.hello(None).await;
    let r = c.request(1, "nope.nothing", json!({})).await;
    assert_eq!(r["error"]["code"], codes::METHOD_NOT_FOUND);

    let mut r2 = router(None);
    r2.add::<apprentice_api::methods::AgentCancel, _, _>(|_c, _p: AgentIdParams| async {
        Ok(Empty {})
    });
    let mut c = start(r2);
    c.hello(None).await;
    let r = c.request(1, "agent.cancel", json!({"agent_id": 5})).await;
    assert_eq!(r["error"]["code"], codes::INVALID_PARAMS);
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("agent.cancel")
    );
}

#[tokio::test]
async fn parse_error_batch_and_oversized_line_keep_the_connection_alive() {
    let mut c = start(router(None));
    c.hello(None).await;

    c.send("{not json").await;
    let r = c.recv().await;
    assert_eq!(r["error"]["code"], codes::PARSE_ERROR);
    assert_eq!(r["id"], Value::Null);

    c.send(r#"[{"jsonrpc":"2.0","id":1,"method":"daemon.status"}]"#)
        .await;
    let r = c.recv().await;
    assert_eq!(r["error"]["code"], codes::INVALID_REQUEST);

    let huge = "x".repeat(MAX_LINE_BYTES + 1);
    c.send(&huge).await;
    let r = c.recv().await;
    assert_eq!(r["error"]["code"], codes::INVALID_REQUEST);
    assert!(r["error"]["message"].as_str().unwrap().contains("exceeds"));

    // Still serving afterwards.
    let r = c.request(7, "daemon.status", json!({})).await;
    assert_eq!(r["id"], 7);
    assert!(r["result"].is_object());
}

#[tokio::test]
async fn handler_panic_becomes_internal_error_and_daemon_keeps_serving() {
    let mut r = router(None);
    r.add::<apprentice_api::methods::AgentCancel, _, _>(|_c, _p: AgentIdParams| async {
        panic!("boom");
        #[allow(unreachable_code)]
        Ok(Empty {})
    });
    let mut c = start(r);
    c.hello(None).await;
    let resp = c.request(1, "agent.cancel", json!({"agent_id":"a"})).await;
    assert_eq!(resp["error"]["code"], codes::INTERNAL_ERROR);
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("panicked")
    );
    let resp = c.request(2, "daemon.status", json!({})).await;
    assert_eq!(resp["id"], 2);
}

#[tokio::test]
async fn concurrent_requests_get_interleaved_responses() {
    // A handler whose latency depends on its input: the later, faster
    // request must be answered first.
    let mut r = router(None);
    r.add::<apprentice_api::methods::AgentCancel, _, _>(|_c, p: AgentIdParams| async move {
        let ms: u64 = p.agent_id.parse().unwrap();
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(Empty {})
    });
    let mut c = start(r);
    c.hello(None).await;
    c.send(
        &json!({"jsonrpc":"2.0","id":1,"method":"agent.cancel","params":{"agent_id":"300"}})
            .to_string(),
    )
    .await;
    c.send(
        &json!({"jsonrpc":"2.0","id":2,"method":"agent.cancel","params":{"agent_id":"10"}})
            .to_string(),
    )
    .await;
    let first = c.recv().await;
    let second = c.recv().await;
    assert_eq!(first["id"], 2);
    assert_eq!(second["id"], 1);
}

#[tokio::test]
async fn streaming_method_emits_ordered_events_per_subscription() {
    let mut r = router(None);
    r.add::<AgentRun, _, _>(|conn: Arc<Connection>, p: AgentRunParams| async move {
        let agent_id = format!("agent-{}", p.prompt);
        let sub = agent_id.clone();
        tokio::spawn(async move {
            for i in 0..3 {
                conn.notify(
                    &sub,
                    Event::AgentTextDelta {
                        agent_id: sub.clone(),
                        text: format!("{i}"),
                    },
                )
                .await
                .unwrap();
            }
            conn.notify(
                &sub,
                Event::AgentFinished {
                    agent_id: sub.clone(),
                    status: AgentStatus::Ok,
                    error: None,
                },
            )
            .await
            .unwrap();
        });
        Ok(AgentRunResult {
            agent_id: agent_id.clone(),
            subscription: agent_id,
        })
    });
    let mut c = start(r);
    c.hello(None).await;
    c.send(&json!({"jsonrpc":"2.0","id":1,"method":"agent.run","params":{"session_id":"s","prompt":"a"}}).to_string()).await;
    c.send(&json!({"jsonrpc":"2.0","id":2,"method":"agent.run","params":{"session_id":"s","prompt":"b"}}).to_string()).await;

    // Collect: 2 responses + 2 x 4 events.
    let mut responses = 0;
    let mut events: std::collections::HashMap<String, Vec<(u64, Value)>> =
        std::collections::HashMap::new();
    for _ in 0..10 {
        let m = c.recv().await;
        if m.get("method").is_some() {
            let p = &m["params"];
            events
                .entry(p["subscription"].as_str().unwrap().to_owned())
                .or_default()
                .push((p["seq"].as_u64().unwrap(), p["event"].clone()));
        } else {
            responses += 1;
        }
    }
    assert_eq!(responses, 2);
    for sub in ["agent-a", "agent-b"] {
        let evs = &events[sub];
        assert_eq!(
            evs.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert_eq!(evs[3].1["type"], "agent.finished");
    }
}

#[tokio::test]
async fn method_registry_lists_names() {
    let r = router(None);
    assert!(r.has(DaemonStatus::NAME));
    assert!(!r.has(AgentRun::NAME));
    assert_eq!(r.methods(), vec![DaemonStatus::NAME]);
    let _ = RpcError::not_found("x"); // error constructors are public
}
