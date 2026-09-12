//! `AnthropicMentor` against a wiremock server replaying SSE fixtures, a
//! raw socket server for interruption/cancellation, and one live test.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use apprentice_api::jsonrpc::{RpcError, codes};
use apprentice_core::config::MentorConfig;
use apprentice_core::mentor::{
    AnthropicMentor, ContentBlock, Mentor, MentorError, MentorRequest, Message, StopReason,
    StreamEvent, SystemBlock, Thinking, ThinkingDisplay, ToolDef,
};
use apprentice_core::secrets::Secret;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn fixture(name: &str) -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sse")
        .join(format!("{name}.txt"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn sse_response(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(fixture(name), "text/event-stream")
}

fn config(base_url: &str) -> MentorConfig {
    MentorConfig {
        base_url: base_url.to_owned(),
        max_retries: 3,
        timeout_s: 30,
        ..MentorConfig::default()
    }
}

fn mentor(base_url: &str) -> AnthropicMentor {
    let cfg = config(base_url);
    AnthropicMentor::new(
        &cfg,
        Secret::new("sk-ant-test"),
        AnthropicMentor::http_client(&cfg).unwrap(),
    )
    .with_max_backoff(Duration::from_millis(20))
}

fn request() -> MentorRequest {
    MentorRequest {
        model: "claude-opus-5".into(),
        max_tokens: 1024,
        system: vec![SystemBlock::new("You are terse.").cached()],
        messages: vec![Message::user("Say hello.")],
        tools: vec![],
        thinking: Thinking::default(),
        effort: apprentice_core::mentor::Effort::High,
        metadata: None,
    }
}

async fn mock(server: &MockServer, template: ResponseTemplate, times: Option<u64>) {
    let m = Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-ant-test"))
        .and(header("anthropic-version", "2023-06-01"))
        .and(header("content-type", "application/json"))
        .respond_with(template);
    match times {
        Some(n) => m.up_to_n_times(n).mount(server).await,
        None => m.mount(server).await,
    }
}

#[derive(Default)]
struct Collected(Arc<Mutex<Vec<StreamEvent>>>);

impl Collected {
    fn sink(&self) -> impl FnMut(StreamEvent) + Send + '_ {
        let v = Arc::clone(&self.0);
        move |e| v.lock().unwrap().push(e)
    }
    fn events(&self) -> Vec<StreamEvent> {
        self.0.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn text_response_streams_and_assembles() {
    let server = MockServer::start().await;
    mock(&server, sse_response("text"), None).await;
    let m = mentor(&server.uri());
    let c = Collected::default();
    let resp = m
        .complete(&request(), &mut c.sink(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(resp.id, "msg_01XFDUDYJgAACzvnptvVoYEL");
    assert_eq!(resp.model, "claude-opus-5");
    assert_eq!(resp.text(), "Hello, world!");
    assert_eq!(resp.stop_reason, StopReason::EndTurn);
    assert_eq!(resp.usage.input_tokens, 25);
    assert_eq!(resp.usage.output_tokens, 12);
    assert_eq!(resp.timing.attempts, 1);
    assert!(resp.request_bytes > 100);
    assert!(resp.raw_sse.is_none());
    let deltas: Vec<_> = c
        .events()
        .into_iter()
        .filter_map(|e| match e {
            StreamEvent::TextDelta(t) => Some(t),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, ["Hello", ", ", "world", "!"]);
    assert_eq!(c.events().last(), Some(&StreamEvent::Done));

    // The request body has the documented shape.
    let req = &server.received_requests().await.unwrap()[0];
    let body: Value = req.body_json().unwrap();
    assert_eq!(body["stream"], true);
    assert_eq!(
        body["thinking"],
        json!({"type": "adaptive", "display": "summarized"})
    );
    assert_eq!(body["output_config"], json!({"effort": "high"}));
    assert_eq!(
        body["system"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(body["messages"][0]["content"][0]["type"], "text");
    assert!(body.get("tools").is_none());
}

#[tokio::test]
async fn parallel_tool_use_and_cache_usage() {
    let server = MockServer::start().await;
    mock(&server, sse_response("tool_use_parallel"), None).await;
    let m = mentor(&server.uri());
    let c = Collected::default();
    let resp = m
        .complete(&request(), &mut c.sink(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(resp.stop_reason, StopReason::ToolUse);
    let uses = resp.tool_uses();
    assert_eq!(uses.len(), 2);
    assert_eq!(uses[0].0, "toolu_01A");
    assert_eq!(uses[0].2, &json!({"path": "src/main.rs"}));
    assert_eq!(uses[1].1, "read_file");
    assert_eq!(uses[1].2, &json!({"path": "Cargo.toml"}));
    assert_eq!(resp.usage.input_tokens, 412);
    assert_eq!(resp.usage.cache_read_input_tokens, 380);
    assert_eq!(resp.usage.output_tokens, 61);
    assert!(c.events().iter().any(|e| matches!(
        e,
        StreamEvent::ToolUseStart { index: 1, id, name } if id == "toolu_01A" && name == "read_file"
    )));
    assert!(c.events().iter().any(|e| matches!(
        e,
        StreamEvent::ToolInputDelta { index: 2, partial_json } if partial_json.contains("Cargo")
    )));
}

#[tokio::test]
async fn thinking_blocks_round_trip_and_usage_from_message_delta() {
    let server = MockServer::start().await;
    mock(&server, sse_response("thinking"), None).await;
    let m = mentor(&server.uri());
    let c = Collected::default();
    let resp = m
        .complete(&request(), &mut c.sink(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(resp.content.len(), 3);
    assert_eq!(
        resp.content[0],
        ContentBlock::Thinking {
            thinking: "The user wants a haiku.".into(),
            signature: "EqQBCkYIBxgCIkD3sig=".into()
        }
    );
    assert!(
        matches!(&resp.content[1], ContentBlock::RedactedThinking { data } if data.starts_with("Emw"))
    );
    assert_eq!(resp.text(), "Silent code compiles\n");
    assert_eq!(resp.usage.cache_creation_input_tokens, 28);
    assert!(
        c.events()
            .iter()
            .any(|e| matches!(e, StreamEvent::ThinkingDelta(t) if t == "The user wants"))
    );

    // Echoing the assistant turn back serialises the blocks unchanged.
    let echoed = serde_json::to_value(Message::assistant(resp.content.clone())).unwrap();
    assert_eq!(
        echoed["content"][0],
        json!({"type": "thinking", "thinking": "The user wants a haiku.", "signature": "EqQBCkYIBxgCIkD3sig="})
    );
    assert_eq!(echoed["content"][1]["type"], "redacted_thinking");

    // A cached-prefix response reports cache reads from either event.
    let server = MockServer::start().await;
    mock(&server, sse_response("cached"), None).await;
    let resp = mentor(&server.uri())
        .complete(&request(), &mut |_| {}, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(resp.usage.cache_read_input_tokens, 2048);
    assert_eq!(resp.usage.input_tokens, 12);
}

#[tokio::test]
async fn max_tokens_and_refusal_stops() {
    let server = MockServer::start().await;
    mock(&server, sse_response("max_tokens"), None).await;
    let resp = mentor(&server.uri())
        .complete(&request(), &mut |_| {}, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(resp.stop_reason, StopReason::MaxTokens);
    assert_eq!(resp.text(), "Once upon a time there");

    let server = MockServer::start().await;
    mock(&server, sse_response("refusal"), None).await;
    let resp = mentor(&server.uri())
        .complete(&request(), &mut |_| {}, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(resp.stop_reason, StopReason::Refusal);
    assert!(resp.content.is_empty());
    let d = resp.stop_details.unwrap();
    assert_eq!(d.category.as_deref(), Some("cyber"));
    assert_eq!(d.explanation, None);
}

#[tokio::test]
async fn error_events_before_and_after_content() {
    // Before content: retried, then succeeds.
    let server = MockServer::start().await;
    mock(&server, sse_response("error_before_content"), Some(1)).await;
    mock(&server, sse_response("text"), None).await;
    let resp = mentor(&server.uri())
        .complete(&request(), &mut |_| {}, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(resp.text(), "Hello, world!");
    assert_eq!(resp.timing.attempts, 2);
    assert_eq!(server.received_requests().await.unwrap().len(), 2);

    // After content: interrupted with the partial content, no retry.
    let server = MockServer::start().await;
    mock(&server, sse_response("error_midstream"), None).await;
    let err = mentor(&server.uri())
        .complete(&request(), &mut |_| {}, CancellationToken::new())
        .await
        .unwrap_err();
    match err {
        MentorError::StreamInterrupted { partial } => {
            assert_eq!(partial[0].as_text(), Some("Partial"));
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn rate_limit_and_overload_are_retried_with_backoff() {
    let server = MockServer::start().await;
    mock(
        &server,
        ResponseTemplate::new(429)
            .insert_header("retry-after", "2")
            .set_body_string(
                r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow"}}"#,
            ),
        Some(1),
    )
    .await;
    mock(
        &server,
        ResponseTemplate::new(529).set_body_string(
            r#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#,
        ),
        Some(1),
    )
    .await;
    mock(&server, sse_response("text"), None).await;

    let m = mentor(&server.uri());
    let resp = m
        .complete(&request(), &mut |_| {}, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(resp.text(), "Hello, world!");
    assert_eq!(resp.timing.attempts, 3);
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn auth_and_invalid_request_are_not_retried() {
    let server = MockServer::start().await;
    mock(
        &server,
        ResponseTemplate::new(401).set_body_string(
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
        ),
        None,
    )
    .await;
    let err = mentor(&server.uri())
        .complete(&request(), &mut |_| {}, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, MentorError::Auth));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let rpc: RpcError = err.into();
    assert_eq!(rpc.code, codes::MENTOR_ERROR);
    let details = rpc.data.unwrap().details.unwrap();
    assert_eq!(details["http_status"], 401);
    assert_eq!(details["api_error_type"], "authentication_error");

    let server = MockServer::start().await;
    mock(
        &server,
        ResponseTemplate::new(400).set_body_string(
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: too big"}}"#,
        ),
        None,
    )
    .await;
    let err = mentor(&server.uri())
        .complete(&request(), &mut |_| {}, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        matches!(&err, MentorError::InvalidRequest { message } if message.contains("max_tokens"))
    );

    // Rate limit exhausted maps to -32021 with retry_after_ms.
    let cfg = MentorConfig {
        max_retries: 0,
        ..config(&server.uri())
    };
    let server = MockServer::start().await;
    mock(
        &server,
        ResponseTemplate::new(429).insert_header("retry-after", "5"),
        None,
    )
    .await;
    let m = AnthropicMentor::new(
        &MentorConfig {
            base_url: server.uri(),
            ..cfg
        },
        Secret::new("sk-ant-test"),
        reqwest::Client::new(),
    );
    let err = m
        .complete(&request(), &mut |_| {}, CancellationToken::new())
        .await
        .unwrap_err();
    let rpc: RpcError = err.into();
    assert_eq!(rpc.code, codes::MENTOR_RATE_LIMITED);
    assert_eq!(rpc.data.unwrap().details.unwrap()["retry_after_ms"], 5000);
}

#[tokio::test]
async fn count_tokens_posts_the_documented_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages/count_tokens"))
        .and(header("x-api-key", "sk-ant-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"input_tokens": 2095})))
        .mount(&server)
        .await;
    let mut req = request();
    req.tools.push(ToolDef {
        name: "read_file".into(),
        description: "Reads a file".into(),
        input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
        cache: apprentice_core::mentor::CacheFlag(true),
    });
    let n = mentor(&server.uri()).count_tokens(&req).await.unwrap();
    assert_eq!(n, 2095);
    let body: Value = server.received_requests().await.unwrap()[0]
        .body_json()
        .unwrap();
    assert!(body.get("stream").is_none());
    assert!(body.get("max_tokens").is_none());
    assert_eq!(body["tools"][0]["name"], "read_file");
    assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(body["messages"][0]["role"], "user");
}

#[tokio::test]
async fn raw_sse_is_captured_on_request() {
    let server = MockServer::start().await;
    mock(&server, sse_response("text"), None).await;
    let m = mentor(&server.uri()).with_raw_sse(true);
    let resp = m
        .complete(&request(), &mut |_| {}, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(resp.raw_sse.unwrap(), fixture("text"));
}

#[test]
fn request_json_matches_reviewed_snapshot() {
    let cfg = MentorConfig::default();
    let m = AnthropicMentor::new(&cfg, Secret::new("k"), reqwest::Client::new());
    let req = MentorRequest {
        model: "claude-opus-5".into(),
        max_tokens: 64_000,
        system: vec![
            SystemBlock::new("You are the mentor."),
            SystemBlock::new("Workspace: /repo").cached(),
        ],
        messages: vec![
            Message::user("Fix the failing test."),
            Message::assistant(vec![
                ContentBlock::Thinking {
                    thinking: "Look at the test first.".into(),
                    signature: "sig==".into(),
                },
                ContentBlock::ToolUse {
                    id: "toolu_1".into(),
                    name: "read_file".into(),
                    input: json!({"path": "tests/x.rs"}),
                    cache: apprentice_core::mentor::CacheFlag(false),
                },
            ]),
            Message {
                role: apprentice_core::mentor::Role::User,
                content: vec![ContentBlock::tool_result("toolu_1", "fn x() {}", false).cached()],
            },
            Message::system("The user enabled auto-approve."),
        ],
        tools: vec![ToolDef {
            name: "read_file".into(),
            description: "Reads a file from the workspace.".into(),
            input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
            cache: apprentice_core::mentor::CacheFlag(true),
        }],
        thinking: Thinking::Adaptive {
            display: ThinkingDisplay::Summarized,
        },
        effort: apprentice_core::mentor::Effort::XHigh,
        metadata: Some(apprentice_core::mentor::Metadata {
            user_id: Some("u-1".into()),
        }),
    };
    let body: Value = serde_json::from_slice(&m.request_body(&req).unwrap()).unwrap();
    insta::assert_json_snapshot!("messages_request", body);
}

// ------------------------------------------------ raw socket scenarios

/// A tiny HTTP/1.1 server: connection `n` gets `scripts[n]` (the last
/// script repeats), each script being the body chunks to write and
/// whether to hang afterwards instead of closing.
async fn raw_server(scripts: Vec<(Vec<Vec<u8>>, bool)>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut conn = 0usize;
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (chunks, hang) = scripts[conn.min(scripts.len() - 1)].clone();
            conn += 1;
            tokio::spawn(async move {
                let mut buf = vec![0u8; 65536];
                let mut n = 0;
                loop {
                    let r = sock.read(&mut buf[n..]).await.unwrap();
                    n += r;
                    if r == 0 || buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let len: usize = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                let body_start = head.find("\r\n\r\n").unwrap() + 4;
                let mut have = n - body_start;
                while have < len {
                    let r = sock.read(&mut buf).await.unwrap();
                    if r == 0 {
                        break;
                    }
                    have += r;
                }
                sock.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
                )
                .await
                .unwrap();
                for c in chunks {
                    sock.write_all(format!("{:x}\r\n", c.len()).as_bytes())
                        .await
                        .unwrap();
                    sock.write_all(&c).await.unwrap();
                    sock.write_all(b"\r\n").await.unwrap();
                    sock.flush().await.unwrap();
                }
                if hang {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                } else {
                    sock.shutdown().await.ok();
                }
            });
        }
    });
    format!("http://{addr}")
}

fn split_fixture(name: &str, at: &str) -> (Vec<u8>, Vec<u8>) {
    let full = fixture(name);
    let text = String::from_utf8(full.clone()).unwrap();
    let idx = text.find(at).unwrap();
    (full[..idx].to_vec(), full[idx..].to_vec())
}

#[tokio::test]
async fn cancellation_mid_stream_is_fast() {
    let (head, _) = split_fixture("text", "event: content_block_stop");
    let url = raw_server(vec![(vec![head], true)]).await;
    let m = mentor(&url);
    let cancel = CancellationToken::new();
    let c = Collected::default();
    let canceller = cancel.clone();
    let events = Arc::clone(&c.0);
    tokio::spawn(async move {
        // Cancel once the first text delta arrived.
        loop {
            if events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, StreamEvent::TextDelta(_)))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        canceller.cancel();
    });
    let started = Instant::now();
    let err = m
        .complete(&request(), &mut c.sink(), cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, MentorError::Cancelled), "{err:?}");
    assert!(
        started.elapsed() < Duration::from_millis(1500),
        "{:?}",
        started.elapsed()
    );
    // Measured from the cancel signal itself the return is well inside 100 ms;
    // the spawned watcher polls every 5 ms.
}

#[tokio::test]
async fn disconnect_after_content_is_interrupted_not_retried() {
    let (head, _) = split_fixture("text", "event: content_block_stop");
    let url = raw_server(vec![(vec![head], false)]).await;
    let err = mentor(&url)
        .complete(&request(), &mut |_| {}, CancellationToken::new())
        .await
        .unwrap_err();
    match err {
        MentorError::StreamInterrupted { partial } => {
            assert_eq!(partial[0].as_text(), Some("Hello, world!"));
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn disconnect_before_content_is_retried() {
    // First connection: message_start only, then close. Second: the full
    // stream. The retry must be transparent to the caller.
    let (head, _) = split_fixture("text", "event: content_block_start");
    let url = raw_server(vec![(vec![head], false), (vec![fixture("text")], false)]).await;
    let resp = mentor(&url)
        .complete(&request(), &mut |_| {}, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(resp.text(), "Hello, world!");
    assert_eq!(resp.timing.attempts, 2);

    // With retries disabled the caller sees the retryable error itself.
    let url = raw_server(vec![(
        vec![split_fixture("text", "event: content_block_start").0],
        false,
    )])
    .await;
    let cfg = MentorConfig {
        max_retries: 0,
        ..config(&url)
    };
    let m = AnthropicMentor::new(&cfg, Secret::new("k"), reqwest::Client::new());
    let err = m
        .complete(&request(), &mut |_| {}, CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, MentorError::Disconnected), "{err:?}");
    assert!(err.is_retryable());
}

/// Real call; run with `HARNESS_LIVE=1 ANTHROPIC_API_KEY=... cargo test -p
/// apprentice-core --test mentor -- --ignored live`.
#[tokio::test]
#[ignore = "spends real tokens; needs HARNESS_LIVE=1 and ANTHROPIC_API_KEY"]
async fn live_small_call() {
    if std::env::var("HARNESS_LIVE").ok().as_deref() != Some("1") {
        eprintln!("HARNESS_LIVE != 1; skipping");
        return;
    }
    let key = std::env::var("ANTHROPIC_API_KEY").expect("ANTHROPIC_API_KEY");
    let cfg = MentorConfig::default();
    let m = AnthropicMentor::new(
        &cfg,
        Secret::new(key),
        AnthropicMentor::http_client(&cfg).unwrap(),
    );
    let req = MentorRequest {
        model: cfg.model.clone(),
        max_tokens: 64,
        system: vec![],
        messages: vec![Message::user("Reply with the single word: pong")],
        tools: vec![],
        thinking: Thinking::Disabled,
        effort: apprentice_core::mentor::Effort::Low,
        metadata: None,
    };
    let n = m.count_tokens(&req).await.unwrap();
    assert!(n > 0 && n < 100, "{n}");
    let resp = m
        .complete(&req, &mut |_| {}, CancellationToken::new())
        .await
        .unwrap();
    assert!(resp.usage.input_tokens > 0);
    assert!(resp.usage.output_tokens > 0);
    assert!(
        resp.text().to_lowercase().contains("pong"),
        "{}",
        resp.text()
    );
    eprintln!("live: {:?} {:?}", resp.usage, resp.timing);
}
