//! The tool system (task M01-01) with mock tools: the execution wrapper
//! (validation, timeout, cancellation, output limits, trace events), the
//! deterministic tool definitions, the parallel policy and `tools.list`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use apprentice_api::methods::ToolsListParams;
use apprentice_api::types::TraceEvent;
use apprentice_core::config::{ConfigLoader, Paths, ToolsConfig};
use apprentice_core::mentor::{ContentBlock, ToolResultContent};
use apprentice_core::tools::{
    AllowAll, Executed, Executor, Gate, Risk, Tool, ToolCall, ToolContext, ToolError, ToolOutput,
    ToolRegistry, ToolResultKind, ToolSpec, ToolsService,
};
use apprentice_core::trace::{
    BlobId, NewAgent, NewSession, StepRef, TraceStore, TraceWriter, kinds,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

// ------------------------------------------------------------- mock tools

/// What a mock does when called.
#[derive(Clone)]
enum Behaviour {
    /// Answer with this text after the delay.
    Text(String, Duration),
    /// Never finish (until cancelled).
    Hang,
    /// Return this error.
    Fail(fn() -> ToolError),
    /// Answer with `is_error` and this diagnostic.
    ErrorResult(String),
    Json(Value),
    Binary(usize),
}

struct MockTool {
    name: &'static str,
    risk: Risk,
    behaviour: Behaviour,
    summary: Option<&'static str>,
    timeout: Option<Duration>,
    log: Arc<Mutex<Vec<String>>>,
}

impl MockTool {
    fn new(name: &'static str, risk: Risk, behaviour: Behaviour) -> Self {
        Self {
            name,
            risk,
            behaviour,
            summary: None,
            timeout: None,
            log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn with_summary(mut self, s: &'static str) -> Self {
        self.summary = Some(s);
        self
    }

    fn with_timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }

    fn with_log(mut self, log: Arc<Mutex<Vec<String>>>) -> Self {
        self.log = log;
        self
    }
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string", "minLength": 1},
            "n": {"type": "integer", "minimum": 0}
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

#[async_trait]
impl Tool for MockTool {
    fn spec(&self) -> ToolSpec {
        let mut spec = ToolSpec::new(
            self.name,
            format!("Mock tool {}.", self.name),
            schema(),
            self.risk,
        )
        .with_tags(["mock"]);
        if let Some(t) = self.timeout {
            spec = spec.with_timeout(t);
        }
        spec
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        assert!(input["path"].as_str().is_some_and(|p| !p.is_empty()));
        self.log
            .lock()
            .unwrap()
            .push(format!("start:{}", self.name));
        let out = match &self.behaviour {
            Behaviour::Text(text, delay) => {
                tokio::select! {
                    () = tokio::time::sleep(*delay) => {}
                    () = cancel.cancelled() => return Err(ToolError::Cancelled),
                }
                ToolOutput::text(text.clone())
            }
            Behaviour::Hang => {
                cancel.cancelled().await;
                self.log
                    .lock()
                    .unwrap()
                    .push(format!("cancelled:{}", self.name));
                return Err(ToolError::Cancelled);
            }
            Behaviour::Fail(make) => return Err(make()),
            Behaviour::ErrorResult(text) => ToolOutput::error(text.clone()),
            Behaviour::Json(v) => ToolOutput::json(v.clone()).with_metadata(json!({"items": 2})),
            Behaviour::Binary(n) => ToolOutput {
                content: apprentice_core::tools::ToolContent::Binary {
                    media_type: "image/png".into(),
                    bytes: vec![0x89; *n],
                },
                summary: String::new(),
                metadata: Value::Null,
                is_error: false,
            },
        };
        assert!(!ctx.call_id.is_empty());
        self.log.lock().unwrap().push(format!("end:{}", self.name));
        Ok(match self.summary {
            Some(s) => out.with_summary(s),
            None => out,
        })
    }
}

// ---------------------------------------------------------------- harness

struct Harness {
    _home: tempfile::TempDir,
    store: Arc<TraceStore>,
    writer: TraceWriter,
    registry: ToolRegistry,
    config: ToolsConfig,
    at: StepRef,
}

impl Harness {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(home.path());
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        let store = Arc::new(TraceStore::open(&paths).unwrap());
        let session = store
            .create_session(&NewSession {
                title: Some("tools".into()),
                workspace_path: None,
                config: json!({}),
            })
            .unwrap();
        let agent = store
            .start_agent(&NewAgent::main(session.clone(), "task"))
            .unwrap();
        let step = store.start_step(&agent).unwrap();
        let writer = TraceWriter::spawn(Arc::clone(&store));
        Self {
            _home: home,
            store,
            writer,
            registry: ToolRegistry::new(),
            config: ToolsConfig::default(),
            at: StepRef {
                session,
                agent,
                step,
            },
        }
    }

    fn add(&self, tool: MockTool) {
        self.registry.register(Arc::new(tool)).unwrap();
    }

    fn executor<'a>(&'a self, gate: &'a dyn Gate, cancel: CancellationToken) -> Executor<'a> {
        Executor::new(
            &self.registry,
            gate,
            &self.writer,
            &self.config,
            self.at.clone(),
            cancel,
        )
    }

    async fn run(&self, call: ToolCall) -> Executed {
        self.executor(&AllowAll, CancellationToken::new())
            .execute_one(call)
            .await
    }

    async fn events(&self) -> Vec<TraceEvent> {
        self.writer.flush().await.unwrap();
        self.store
            .session_events(&self.at.session)
            .unwrap()
            .into_iter()
            .filter(|e| e.summary.kind.starts_with("tool."))
            .collect()
    }

    /// `(tool.call, tool.result)` for `call_id`.
    async fn pair(&self, call_id: &str) -> (TraceEvent, TraceEvent) {
        let events = self.events().await;
        let find = |kind: &str| {
            events
                .iter()
                .find(|e| e.summary.kind == kind && e.payload["call_id"] == call_id)
                .cloned()
                .unwrap_or_else(|| panic!("no {kind} for {call_id}"))
        };
        (find(kinds::TOOL_CALL), find(kinds::TOOL_RESULT))
    }

    async fn close(self) {
        self.writer.shutdown().await;
    }
}

fn text_of(block: &ContentBlock) -> (String, bool) {
    match block {
        ContentBlock::ToolResult {
            content, is_error, ..
        } => {
            let text = content
                .iter()
                .map(|c| match c {
                    ToolResultContent::Text { text } => text.clone(),
                    other => panic!("unexpected content {other:?}"),
                })
                .collect();
            (text, *is_error)
        }
        other => panic!("not a tool_result: {other:?}"),
    }
}

fn call(id: &str, name: &str, input: Value) -> ToolCall {
    ToolCall::new(id, name, input)
}

// ------------------------------------------------------------------ tests

#[tokio::test]
async fn round_trip_records_both_events_and_returns_the_output() {
    let h = Harness::new();
    h.add(
        MockTool::new(
            "read",
            Risk::ReadOnly,
            Behaviour::Text("hello, world".into(), Duration::ZERO),
        )
        .with_summary("read 1 line of a.txt"),
    );
    let out = h.run(call("t1", "read", json!({"path": "a.txt"}))).await;
    assert_eq!(out.kind, ToolResultKind::Ok);
    assert!(out.ok);
    assert_eq!(out.summary, "read 1 line of a.txt");
    assert_eq!(text_of(&out.block), ("hello, world".into(), false));
    assert_eq!(out.output_bytes, 12);
    assert_eq!(out.mentor_bytes, 12);
    assert!(!out.truncated);
    assert!(out.result_event.is_some());

    let (c, r) = h.pair("t1").await;
    assert_eq!(c.payload["name"], "read");
    assert_eq!(c.payload["risk"], "read_only");
    assert_eq!(c.payload["input_bytes"], 16);
    assert_eq!(
        h.store
            .read_blob(&BlobId::from(c.blob_id.as_deref().unwrap()))
            .unwrap(),
        br#"{"path":"a.txt"}"#
    );
    assert_eq!(c.summary.step_id.as_deref(), Some(h.at.step.as_str()));
    assert_eq!(r.payload["ok"], true);
    assert_eq!(r.payload["kind"], "ok");
    assert_eq!(r.payload["output_bytes"], 12);
    assert_eq!(r.payload["mentor_bytes"], 12);
    assert_eq!(r.payload["truncated"], false);
    assert_eq!(r.payload["summary"], "read 1 line of a.txt");
    assert_eq!(r.payload["media_type"], "text/plain; charset=utf-8");
    assert!(r.payload["duration_ms"].is_u64());
    let blob = h.store.read_blob(&out.blob_id.clone().unwrap()).unwrap();
    assert_eq!(blob, b"hello, world");
    assert_eq!(r.blob_id.as_deref(), Some(out.blob_id.unwrap().as_str()));
    h.close().await;
}

#[tokio::test]
async fn schema_failure_is_an_error_result_without_running_the_tool() {
    let h = Harness::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    h.add(
        MockTool::new(
            "read",
            Risk::ReadOnly,
            Behaviour::Text("x".into(), Duration::ZERO),
        )
        .with_log(Arc::clone(&log)),
    );
    let out = h
        .run(call("t2", "read", json!({"n": -1, "bogus": 1})))
        .await;
    assert_eq!(out.kind, ToolResultKind::InvalidInput);
    assert!(!out.ok);
    let (text, is_error) = text_of(&out.block);
    assert!(is_error);
    assert!(text.starts_with("invalid input for read: "), "{text}");
    assert!(text.contains("\"path\" is a required property"), "{text}");
    assert!(
        text.contains("/n: -1 is less than the minimum of 0"),
        "{text}"
    );
    assert!(text.contains("bogus"), "{text}");
    assert_eq!(out.summary, "read: invalid input");
    assert!(out.blob_id.is_none());
    assert!(log.lock().unwrap().is_empty(), "tool must not run");

    let (_, r) = h.pair("t2").await;
    assert_eq!(r.payload["ok"], false);
    assert_eq!(r.payload["kind"], "invalid_input");
    assert!(
        r.payload["message"]
            .as_str()
            .unwrap()
            .contains("invalid input")
    );
    assert!(r.blob_id.is_none());
    h.close().await;
}

#[tokio::test]
async fn unknown_tool_is_an_error_result() {
    let h = Harness::new();
    h.add(MockTool::new(
        "read",
        Risk::ReadOnly,
        Behaviour::Text("x".into(), Duration::ZERO),
    ));
    let out = h.run(call("t3", "nope", json!({}))).await;
    assert_eq!(out.kind, ToolResultKind::InvalidInput);
    let (text, is_error) = text_of(&out.block);
    assert!(is_error);
    assert_eq!(
        text,
        "invalid input for nope: unknown tool \"nope\"; available: read"
    );
    let (c, r) = h.pair("t3").await;
    assert!(c.payload["risk"].is_null());
    assert_eq!(r.payload["kind"], "invalid_input");
    h.close().await;
}

#[tokio::test]
async fn timeout_is_an_error_result_and_the_tool_is_told() {
    let h = Harness::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    h.add(
        MockTool::new("slow", Risk::Execute, Behaviour::Hang)
            .with_timeout(Duration::from_secs(1))
            .with_log(Arc::clone(&log)),
    );
    let out = h.run(call("t4", "slow", json!({"path": "p"}))).await;
    assert_eq!(out.kind, ToolResultKind::Timeout);
    let (text, is_error) = text_of(&out.block);
    assert!(is_error);
    assert_eq!(text, "slow timed out after 1 s");
    assert_eq!(out.summary, "slow: timed out after 1 s");
    assert!(out.duration >= Duration::from_secs(1));

    let (_, r) = h.pair("t4").await;
    assert_eq!(r.payload["ok"], false);
    assert_eq!(r.payload["kind"], "timeout");
    assert_eq!(r.payload["risk"], "execute");
    // The abandoned future was dropped; its child token was cancelled
    // first so a tool that does honour it can clean up.
    tokio::task::yield_now().await;
    let log = log.lock().unwrap().clone();
    assert_eq!(log, ["start:slow"]);
    h.close().await;
}

#[tokio::test]
async fn cancellation_gives_a_cancelled_result() {
    let h = Harness::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    h.add(MockTool::new("hang", Risk::ReadOnly, Behaviour::Hang).with_log(Arc::clone(&log)));
    let cancel = CancellationToken::new();
    let exec = h.executor(&AllowAll, cancel.clone());
    let run = exec.execute_one(call("t5", "hang", json!({"path": "p"})));
    let canceller = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
    };
    let (out, ()) = tokio::join!(run, canceller);
    assert_eq!(out.kind, ToolResultKind::Cancelled);
    assert!(out.is_cancelled());
    assert_eq!(text_of(&out.block), ("cancelled".into(), true));
    let (_, r) = h.pair("t5").await;
    assert_eq!(r.payload["kind"], "cancelled");

    // Already cancelled: the tool does not even start.
    let out = exec
        .execute_one(call("t6", "hang", json!({"path": "p"})))
        .await;
    assert_eq!(out.kind, ToolResultKind::Cancelled);
    assert_eq!(
        log.lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with("start"))
            .count(),
        1
    );
    h.close().await;
}

#[tokio::test]
async fn large_output_is_stored_whole_and_cut_for_the_mentor() {
    let mut h = Harness::new();
    h.config.max_mentor_bytes = 1000;
    let big = (0..500).fold(String::new(), |mut s, i| {
        use std::fmt::Write as _;
        let _ = writeln!(s, "{i:04} ééé");
        s
    });
    assert!(big.len() > 5000);
    h.add(MockTool::new(
        "big",
        Risk::ReadOnly,
        Behaviour::Text(big.clone(), Duration::ZERO),
    ));
    let out = h.run(call("t7", "big", json!({"path": "p"}))).await;
    assert_eq!(out.kind, ToolResultKind::Ok);
    assert!(out.truncated);
    assert_eq!(out.output_bytes, big.len());
    let blob_id = out.blob_id.clone().unwrap();
    assert_eq!(h.store.read_blob(&blob_id).unwrap(), big.as_bytes());

    let (text, is_error) = text_of(&out.block);
    assert!(!is_error);
    assert!(text.starts_with("0000 ééé\n0001 ééé\n"), "{text}");
    assert!(text.ends_with("0498 ééé\n0499 ééé\n"), "{text}");
    let marker = text
        .lines()
        .find(|l| l.starts_with("[... "))
        .unwrap_or_else(|| panic!("no marker in {text}"));
    assert!(
        marker.ends_with(&format!("bytes omitted, full result id {blob_id}]")),
        "{marker}"
    );
    let omitted: usize = marker["[... ".len()..]
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    // head, the marker line, tail: head + omitted + tail is the whole
    // output (the head gets a newline when the cut is mid-line).
    let idx = text.find(marker).unwrap();
    let (head, tail) = (&text[..idx], &text[idx + marker.len() + 1..]);
    let head = if big.starts_with(head) {
        head
    } else {
        head.strip_suffix('\n').unwrap()
    };
    assert!(big.starts_with(head));
    assert!(big.ends_with(tail));
    let kept = head.len() + tail.len();
    assert_eq!(kept + omitted, big.len());
    assert!(kept <= 1000, "{kept}");
    assert!(kept > 900, "{kept}");
    assert_eq!(out.mentor_bytes, text.len());
    assert_eq!(out.summary, format!("big: {} bytes", big.len()));

    let (_, r) = h.pair("t7").await;
    assert_eq!(r.payload["truncated"], true);
    assert_eq!(r.payload["output_bytes"], big.len());
    assert_eq!(r.payload["mentor_bytes"], text.len());
    assert!(r.payload.get("truncated_at_capture").is_none());
    h.close().await;
}

#[tokio::test]
async fn output_beyond_the_capture_limit_keeps_head_and_tail() {
    let mut h = Harness::new();
    h.config.max_capture_bytes = 100;
    h.config.max_mentor_bytes = 40;
    let big = "abcdefghij".repeat(30); // 300 bytes
    h.add(MockTool::new(
        "big",
        Risk::ReadOnly,
        Behaviour::Text(big.clone(), Duration::ZERO),
    ));
    let out = h.run(call("t8", "big", json!({"path": "p"}))).await;
    let blob = h.store.read_blob(&out.blob_id.clone().unwrap()).unwrap();
    assert_eq!(blob.len(), 100);
    assert_eq!(&blob[..75], &big.as_bytes()[..75]);
    assert_eq!(&blob[75..], &big.as_bytes()[275..]);
    assert_eq!(out.output_bytes, 300);
    let (_, r) = h.pair("t8").await;
    assert_eq!(r.payload["truncated_at_capture"], true);
    assert_eq!(r.payload["output_bytes"], 300);
    assert_eq!(r.payload["truncated"], true);
    h.close().await;
}

#[tokio::test]
async fn error_results_json_and_binary_content() {
    let h = Harness::new();
    h.add(MockTool::new(
        "cargo",
        Risk::Execute,
        Behaviour::ErrorResult("error[E0425]: cannot find value".into()),
    ));
    h.add(MockTool::new(
        "list",
        Risk::ReadOnly,
        Behaviour::Json(json!({"files": ["a", "b"]})),
    ));
    h.add(MockTool::new(
        "shot",
        Risk::ReadOnly,
        Behaviour::Binary(300),
    ));
    h.add(MockTool::new(
        "empty",
        Risk::ReadOnly,
        Behaviour::Text(String::new(), Duration::ZERO),
    ));
    h.add(MockTool::new(
        "broken",
        Risk::ReadOnly,
        Behaviour::Fail(|| ToolError::Failed("no such binary".into())),
    ));

    let out = h.run(call("e1", "cargo", json!({"path": "p"}))).await;
    assert_eq!(out.kind, ToolResultKind::Error);
    assert!(!out.ok);
    assert_eq!(
        text_of(&out.block),
        ("error[E0425]: cannot find value".into(), true)
    );
    assert_eq!(out.summary, "cargo: error, 31 bytes");
    assert!(out.blob_id.is_some(), "the diagnostic is captured too");
    let (_, r) = h.pair("e1").await;
    assert_eq!(r.payload["ok"], false);
    assert_eq!(r.payload["kind"], "error");

    let out = h.run(call("e2", "list", json!({"path": "p"}))).await;
    assert_eq!(out.kind, ToolResultKind::Ok);
    let (text, _) = text_of(&out.block);
    assert_eq!(text, "{\n  \"files\": [\n    \"a\",\n    \"b\"\n  ]\n}");
    let (_, r) = h.pair("e2").await;
    assert_eq!(r.payload["media_type"], "application/json");
    assert_eq!(r.payload["metadata"], json!({"items": 2}));
    assert_eq!(
        h.store.read_blob(&out.blob_id.unwrap()).unwrap(),
        br#"{"files":["a","b"]}"#
    );

    let out = h.run(call("e3", "shot", json!({"path": "p"}))).await;
    let (text, _) = text_of(&out.block);
    assert_eq!(
        text,
        format!(
            "[binary result: image/png, 300 bytes, full result id {}]",
            out.blob_id.as_ref().unwrap()
        )
    );
    assert!(!out.truncated);
    let (_, r) = h.pair("e3").await;
    assert_eq!(r.payload["media_type"], "image/png");
    assert_eq!(r.payload["output_bytes"], 300);

    let out = h.run(call("e4", "empty", json!({"path": "p"}))).await;
    assert_eq!(text_of(&out.block), ("(no output)".into(), false));
    assert_eq!(out.output_bytes, 0);
    assert_eq!(out.summary, "empty: 0 bytes");

    let out = h.run(call("e5", "broken", json!({"path": "p"}))).await;
    assert_eq!(out.kind, ToolResultKind::Failed);
    assert_eq!(
        text_of(&out.block),
        ("broken failed: no such binary".into(), true)
    );
    assert_eq!(out.summary, "broken: failed: no such binary");
    h.close().await;
}

#[tokio::test]
async fn summaries_are_normalised_to_one_short_line() {
    let h = Harness::new();
    h.add(
        MockTool::new(
            "chatty",
            Risk::ReadOnly,
            Behaviour::Text("x".into(), Duration::ZERO),
        )
        .with_summary("  first line\nsecond   line that goes on and on and on and on and on and on and on and on and on and on and on and on and on and on"),
    );
    let out = h.run(call("s1", "chatty", json!({"path": "p"}))).await;
    assert!(!out.summary.contains('\n'));
    assert_eq!(out.summary.chars().count(), 120);
    assert!(out.summary.starts_with("first line second line"));
    assert!(out.summary.ends_with('…'));
    h.close().await;
}

struct DenyWrites;

#[async_trait]
impl Gate for DenyWrites {
    async fn permit(&self, _: &ToolContext, spec: &ToolSpec, _: &Value) -> Result<(), String> {
        if spec.is_mutating() {
            Err("denied by user".into())
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn the_gate_can_deny_a_call() {
    let h = Harness::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    h.add(
        MockTool::new(
            "write",
            Risk::Write,
            Behaviour::Text("written".into(), Duration::ZERO),
        )
        .with_log(Arc::clone(&log)),
    );
    h.add(MockTool::new(
        "read",
        Risk::ReadOnly,
        Behaviour::Text("content".into(), Duration::ZERO),
    ));
    let exec = h.executor(&DenyWrites, CancellationToken::new());
    let out = exec
        .execute_all(vec![
            call("g1", "write", json!({"path": "p"})),
            call("g2", "read", json!({"path": "p"})),
        ])
        .await;
    assert_eq!(out[0].kind, ToolResultKind::Denied);
    assert_eq!(text_of(&out[0].block), ("denied by user".into(), true));
    assert_eq!(out[0].summary, "write: denied");
    assert_eq!(out[1].kind, ToolResultKind::Ok);
    assert!(log.lock().unwrap().is_empty());
    let (_, r) = h.pair("g1").await;
    assert_eq!(r.payload["kind"], "denied");
    h.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_only_calls_run_concurrently_and_writes_after_them_in_order() {
    let h = Harness::new();
    let log = Arc::new(Mutex::new(Vec::new()));
    let delay = Duration::from_millis(150);
    for name in ["r1", "r2"] {
        h.add(
            MockTool::new(name, Risk::ReadOnly, Behaviour::Text(name.into(), delay))
                .with_log(Arc::clone(&log)),
        );
    }
    h.add(
        MockTool::new("w1", Risk::Write, Behaviour::Text("w1".into(), delay))
            .with_log(Arc::clone(&log)),
    );
    h.add(
        MockTool::new(
            "x1",
            Risk::Execute,
            Behaviour::Text("x1".into(), Duration::ZERO),
        )
        .with_log(Arc::clone(&log)),
    );
    let exec = h.executor(&AllowAll, CancellationToken::new());
    let started = std::time::Instant::now();
    let out = exec
        .execute_all(vec![
            call("p1", "w1", json!({"path": "p"})),
            call("p2", "r1", json!({"path": "p"})),
            call("p3", "x1", json!({"path": "p"})),
            call("p4", "r2", json!({"path": "p"})),
        ])
        .await;
    let elapsed = started.elapsed();
    // Original order, whatever ran first.
    assert_eq!(
        out.iter().map(|e| e.call_id.as_str()).collect::<Vec<_>>(),
        ["p1", "p2", "p3", "p4"]
    );
    assert_eq!(
        out.iter().map(|e| text_of(&e.block).0).collect::<Vec<_>>(),
        ["w1", "r1", "x1", "r2"]
    );
    assert!(out.iter().all(|e| e.ok));

    let log = log.lock().unwrap().clone();
    let pos = |s: &str| {
        log.iter()
            .position(|l| l == s)
            .unwrap_or_else(|| panic!("{s} in {log:?}"))
    };
    // Both reads started before either ended: concurrent.
    assert!(pos("start:r1") < pos("end:r1") && pos("start:r1") < pos("end:r2"));
    assert!(pos("start:r2") < pos("end:r1") && pos("start:r2") < pos("end:r2"));
    // The write started after both reads ended, the execute after the write.
    assert!(pos("start:w1") > pos("end:r1") && pos("start:w1") > pos("end:r2"));
    assert!(pos("start:x1") > pos("end:w1"));
    // Two sequential delays plus one concurrent pair, not four delays.
    assert!(elapsed < delay * 4, "{elapsed:?}");
    assert!(elapsed >= delay * 2, "{elapsed:?}");

    // The trace has every call, in the order the mentor asked, before any
    // result of a later phase.
    let events = h.events().await;
    let calls: Vec<_> = events
        .iter()
        .filter(|e| e.summary.kind == kinds::TOOL_CALL)
        .map(|e| e.payload["call_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(calls.len(), 4);
    assert_eq!(events.len(), 8);
    h.close().await;
}

#[test]
fn tool_definitions_are_deterministic() {
    let reg = ToolRegistry::new();
    for (name, risk) in [
        ("write_file", Risk::Write),
        ("read_file", Risk::ReadOnly),
        ("shell", Risk::Execute),
        ("grep", Risk::ReadOnly),
    ] {
        reg.register(Arc::new(MockTool::new(
            name,
            risk,
            Behaviour::Text(String::new(), Duration::ZERO),
        )))
        .unwrap();
    }
    let defs = reg.defs(&[]);
    assert_eq!(
        defs.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        ["grep", "read_file", "shell", "write_file"]
    );
    let once = serde_json::to_vec(&defs).unwrap();
    let again = serde_json::to_vec(&reg.defs(&[])).unwrap();
    assert_eq!(once, again);
    insta::assert_json_snapshot!("tool_defs", defs);
    insta::assert_json_snapshot!("tools_list", reg.list(&["shell".into()]));
}

#[tokio::test]
async fn tools_list_reflects_the_workspace_config() {
    let home = tempfile::tempdir().unwrap();
    let paths = Paths::from_home(home.path());
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::write(paths.config_file(), "[tools]\ndisabled = [\"shell\"]\n").unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let ws_config = Paths::workspace_config_file(workspace.path());
    std::fs::create_dir_all(ws_config.parent().unwrap()).unwrap();
    std::fs::write(
        &ws_config,
        "[tools]\ndisabled = [\"write_file\", \"grep\"]\n",
    )
    .unwrap();

    let registry = Arc::new(ToolRegistry::new());
    for name in ["shell", "grep", "write_file", "read_file"] {
        registry
            .register(Arc::new(MockTool::new(
                name,
                Risk::ReadOnly,
                Behaviour::Text(String::new(), Duration::ZERO),
            )))
            .unwrap();
    }
    let loader = ConfigLoader::new(paths);
    assert_eq!(
        loader.load(None).unwrap().config.tools,
        ToolsConfig {
            disabled: vec!["shell".into()],
            ..ToolsConfig::default()
        }
    );
    let svc = ToolsService::new(registry, loader);

    let enabled = |r: apprentice_api::methods::ToolsListResult| {
        r.tools
            .iter()
            .map(|t| (t.name.clone(), t.enabled))
            .collect::<Vec<_>>()
    };
    let user = svc.list(&ToolsListParams::default()).unwrap();
    assert_eq!(
        enabled(user),
        [
            ("grep".to_owned(), true),
            ("read_file".to_owned(), true),
            ("shell".to_owned(), false),
            ("write_file".to_owned(), true),
        ]
    );
    let ws = svc
        .list(&ToolsListParams {
            workspace: Some(workspace.path().to_string_lossy().into_owned()),
        })
        .unwrap();
    assert_eq!(
        enabled(ws),
        [
            ("grep".to_owned(), false),
            ("read_file".to_owned(), true),
            ("shell".to_owned(), true),
            ("write_file".to_owned(), false),
        ]
    );
}
