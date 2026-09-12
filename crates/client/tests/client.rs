//! `DaemonClient` against the real `Router`: over an in-memory duplex and
//! over a real local socket (named pipe on Windows, Unix socket elsewhere).

use std::sync::Arc;
use std::time::Duration;

use apprentice_api::events::{AgentStatus, Event, LogLevel};
use apprentice_api::jsonrpc::codes;
use apprentice_api::methods::{
    AgentCancel, AgentIdParams, AgentRun, AgentRunParams, AgentRunResult, DaemonStatus,
    DaemonStatusResult, Empty,
};
use apprentice_api::server::{Connection, Router, RouterConfig};
use apprentice_api::transport::Endpoint;
use apprentice_api::types::RunOptions;
use apprentice_client::{ClientError, ClientOptions, DaemonClient};

fn router(token: Option<&str>) -> Router {
    let mut r = Router::new(RouterConfig {
        daemon_version: "1.2.3".into(),
        pid: 7,
        token: token.map(str::to_owned),
    });
    r.add::<DaemonStatus, _, _>(|_c, Empty {}| async {
        Ok(DaemonStatusResult {
            version: "1.2.3".into(),
            pid: 7,
            uptime_s: 0,
            sessions_open: 0,
            data_dir: "d".into(),
            log_file: None,
        })
    });
    // Sleeps for `agent_id` milliseconds; used to test interleaving.
    r.add::<AgentCancel, _, _>(|_c, p: AgentIdParams| async move {
        let ms: u64 = p.agent_id.parse().unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(Empty {})
    });
    // Streams `prompt.len()` text deltas then finishes. `prompt` starting
    // with "burst" emits before returning the result, to exercise buffering.
    r.add::<AgentRun, _, _>(|conn: Arc<Connection>, p: AgentRunParams| async move {
        let sub = format!("agent-{}", p.prompt);
        let n = p.prompt.len();
        let emit = {
            let conn = Arc::clone(&conn);
            let sub = sub.clone();
            async move {
                for i in 0..n {
                    conn.notify(
                        &sub,
                        Event::AgentTextDelta {
                            agent_id: sub.clone(),
                            text: i.to_string(),
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
                        truncated: false,
                    },
                )
                .await
                .unwrap();
            }
        };
        if p.prompt.starts_with("burst") {
            emit.await;
        } else {
            tokio::spawn(emit);
        }
        Ok(AgentRunResult {
            agent_id: sub.clone(),
            subscription: sub,
        })
    });
    r
}

fn start(router: Router, options: ClientOptions) -> (DaemonClient, tokio::task::JoinHandle<()>) {
    let (server_side, client_side) = tokio::io::duplex(1 << 20);
    let (sr, sw) = tokio::io::split(server_side);
    let router = Arc::new(router);
    let server = tokio::spawn(async move {
        let _ = router.serve(sr, sw).await;
    });
    let (cr, cw) = tokio::io::split(client_side);
    (DaemonClient::from_streams(cr, cw, options), server)
}

#[tokio::test]
async fn hello_then_typed_call() {
    let (client, _server) = start(router(Some("tok")), ClientOptions::default());
    let err = client
        .hello("test", "0", Some("bad".into()))
        .await
        .unwrap_err();
    match err {
        ClientError::Rpc(e) => assert_eq!(e.code, codes::UNAUTHORIZED),
        other => panic!("unexpected {other:?}"),
    }
    let hello = client.hello("test", "0", Some("tok".into())).await.unwrap();
    assert_eq!(hello.daemon_version, "1.2.3");
    let status = client.call::<DaemonStatus>(Empty {}).await.unwrap();
    assert_eq!(status.pid, 7);
}

#[tokio::test]
async fn dropping_the_last_handle_closes_the_connection() {
    let (client, server) = start(router(None), ClientOptions::default());
    client.hello("t", "0", None).await.unwrap();
    let other = client.clone();
    drop(client);
    // One handle still alive: the daemon keeps serving.
    other.call::<DaemonStatus>(Empty {}).await.unwrap();
    assert!(!server.is_finished());
    drop(other);
    // None left: the daemon sees EOF and its serve() returns.
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server did not observe the close")
        .unwrap();
}

#[tokio::test]
async fn concurrent_calls_are_matched_by_id() {
    let (client, _server) = start(router(None), ClientOptions::default());
    client.hello("t", "0", None).await.unwrap();
    let slow = client.call::<AgentCancel>(AgentIdParams {
        agent_id: "200".into(),
    });
    let fast = client.call::<AgentCancel>(AgentIdParams {
        agent_id: "5".into(),
    });
    let status = client.call::<DaemonStatus>(Empty {});
    let (a, b, c) = tokio::join!(slow, fast, status);
    a.unwrap();
    b.unwrap();
    assert_eq!(c.unwrap().version, "1.2.3");
}

#[tokio::test]
async fn two_subscriptions_do_not_cross() {
    let (client, _server) = start(router(None), ClientOptions::default());
    client.hello("t", "0", None).await.unwrap();
    let (ra, mut sa) = client
        .call_streaming::<AgentRun>(AgentRunParams {
            session_id: "s".into(),
            prompt: "aaa".into(),
            options: RunOptions::default(),
        })
        .await
        .unwrap();
    let (rb, mut sb) = client
        .call_streaming::<AgentRun>(AgentRunParams {
            session_id: "s".into(),
            prompt: "bb".into(),
            options: RunOptions::default(),
        })
        .await
        .unwrap();
    assert_eq!(ra.subscription, "agent-aaa");
    assert_eq!(rb.subscription, "agent-bb");

    let mut got_a = Vec::new();
    while let Some(ev) = sa.next().await {
        assert_eq!(ev.subscription, "agent-aaa");
        got_a.push(ev);
    }
    let mut got_b = Vec::new();
    while let Some(ev) = sb.next().await {
        assert_eq!(ev.subscription, "agent-bb");
        got_b.push(ev);
    }
    assert_eq!(got_a.len(), 4);
    assert_eq!(got_b.len(), 3);
    assert!(got_a.last().unwrap().event.is_terminal());
    assert_eq!(
        got_a.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
}

#[tokio::test]
async fn events_emitted_before_subscribe_are_not_lost() {
    let (client, _server) = start(router(None), ClientOptions::default());
    client.hello("t", "0", None).await.unwrap();
    // The server emits all events before answering the request.
    let (_r, mut s) = client
        .call_streaming::<AgentRun>(AgentRunParams {
            session_id: "s".into(),
            prompt: "burst".into(),
            options: RunOptions::default(),
        })
        .await
        .unwrap();
    let mut n = 0;
    while let Some(ev) = s.next().await {
        n += 1;
        if n == 6 {
            assert!(ev.event.is_terminal());
        }
    }
    assert_eq!(n, 6); // 5 deltas + finished
}

#[tokio::test]
async fn slow_consumer_gets_a_dropped_warning_not_a_hang() {
    let (client, _server) = start(
        router(None),
        ClientOptions {
            timeout: None,
            event_buffer: 2,
        },
    );
    client.hello("t", "0", None).await.unwrap();
    let (_r, mut s) = client
        .call_streaming::<AgentRun>(AgentRunParams {
            session_id: "s".into(),
            prompt: "x".repeat(50),
            options: RunOptions::default(),
        })
        .await
        .unwrap();
    // Let the daemon flood while we are not reading.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut warned = false;
    let mut count = 0;
    let mut last = None;
    while let Some(ev) = s.next().await {
        count += 1;
        if let Event::Log {
            level: LogLevel::Warn,
            message,
        } = &ev.event
        {
            assert!(message.contains("dropped"));
            warned = true;
        }
        last = Some(ev);
    }
    assert!(count < 51, "buffer should have dropped events");
    assert!(warned, "a warning should announce the drop");
    assert!(
        last.unwrap().event.is_terminal(),
        "the terminal event is never dropped"
    );
}

#[tokio::test]
async fn server_closing_mid_request_returns_closed() {
    let (client, server) = start(router(None), ClientOptions::default());
    client.hello("t", "0", None).await.unwrap();
    let pending = client.call::<AgentCancel>(AgentIdParams {
        agent_id: "5000".into(),
    });
    let killer = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        server.abort();
    };
    let (res, ()) = tokio::join!(pending, killer);
    assert!(matches!(res, Err(ClientError::Closed)), "got {res:?}");
    assert!(client.is_closed());
    assert!(matches!(
        client.call::<DaemonStatus>(Empty {}).await,
        Err(ClientError::Closed)
    ));
}

#[tokio::test]
async fn closed_resolves_when_the_server_goes_away() {
    let (client, server) = start(router(None), ClientOptions::default());
    client.hello("t", "0", None).await.unwrap();
    let waiter = client.clone();
    let waiting = tokio::spawn(async move { waiter.closed().await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !waiting.is_finished(),
        "closed() resolved on an open connection"
    );
    server.abort();
    tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("closed() did not resolve")
        .unwrap();
    assert!(client.is_closed());
    // Already closed: returns at once.
    tokio::time::timeout(Duration::from_millis(100), client.closed())
        .await
        .expect("closed() must not wait on a closed connection");
}

#[tokio::test]
async fn call_times_out() {
    let (client, _server) = start(
        router(None),
        ClientOptions {
            timeout: Some(Duration::from_millis(50)),
            event_buffer: 8,
        },
    );
    client.hello("t", "0", None).await.unwrap();
    let res = client
        .call::<AgentCancel>(AgentIdParams {
            agent_id: "1000".into(),
        })
        .await;
    assert!(matches!(res, Err(ClientError::Timeout(_))), "got {res:?}");
}

#[tokio::test]
async fn local_socket_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let endpoint = if cfg!(windows) {
        Endpoint::Namespaced(format!("apprentice-harness-test-{}", std::process::id()))
    } else {
        Endpoint::Path(tmp.path().join("daemon.sock"))
    };
    let listener = endpoint.listen().unwrap();
    let router = Arc::new(router(Some("tok")));
    let accept_loop = tokio::spawn(async move {
        loop {
            let (r, w) = listener.accept().await.unwrap();
            let router = Arc::clone(&router);
            tokio::spawn(async move {
                let _ = router.serve(r, w).await;
            });
        }
    });

    let client = DaemonClient::connect_endpoint(&endpoint, ClientOptions::default())
        .await
        .unwrap();
    let hello = client
        .hello("sock-test", "0", Some("tok".into()))
        .await
        .unwrap();
    assert_eq!(hello.pid, 7);
    let (_r, mut s) = client
        .call_streaming::<AgentRun>(AgentRunParams {
            session_id: "s".into(),
            prompt: "hi".into(),
            options: RunOptions::default(),
        })
        .await
        .unwrap();
    let mut n = 0;
    while s.next().await.is_some() {
        n += 1;
    }
    assert_eq!(n, 3);

    // A second client on the same endpoint works independently.
    let client2 = DaemonClient::connect_endpoint(&endpoint, ClientOptions::default())
        .await
        .unwrap();
    client2
        .hello("sock-test-2", "0", Some("tok".into()))
        .await
        .unwrap();
    assert_eq!(client2.call::<DaemonStatus>(Empty {}).await.unwrap().pid, 7);

    accept_loop.abort();
}

#[tokio::test]
async fn connect_to_missing_endpoint_fails_fast() {
    let endpoint = if cfg!(windows) {
        Endpoint::Namespaced("apprentice-harness-nobody-listens-here".into())
    } else {
        Endpoint::Path(std::env::temp_dir().join("apprentice-harness-nobody.sock"))
    };
    let res = tokio::time::timeout(
        Duration::from_secs(5),
        DaemonClient::connect_endpoint(&endpoint, ClientOptions::default()),
    )
    .await
    .expect("connect should not hang");
    assert!(matches!(res, Err(ClientError::Io(_))));
}
