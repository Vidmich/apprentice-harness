//! The agent loop end to end (task M01-08): the real `harness` CLI
//! against the real `harnessd`, whose mentor is a wiremock server
//! playing the three-step trajectory (read two files in parallel,
//! write one, answer). Every step and tool call lands in the trace as
//! `harness trace list` shows it, the file is written, and a second
//! `run --session` continues the same conversation.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use apprentice_client::DaemonInfo;
use assert_cmd::assert::Assert;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const HARNESSD: &str = env!("CARGO_BIN_EXE_harnessd");
const API_KEY: &str = "sk-ant-e2e";

fn harness_bin() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let bin = Path::new(HARNESSD)
            .parent()
            .unwrap()
            .join(format!("harness{}", std::env::consts::EXE_SUFFIX));
        if !bin.is_file() {
            let status = Command::new(env!("CARGO"))
                .args(["build", "-p", "harness", "--bin", "harness"])
                .status()
                .expect("cargo build");
            assert!(status.success(), "building harness failed");
        }
        assert!(bin.is_file(), "{} missing", bin.display());
        bin
    })
}

fn sse(name: &str) -> ResponseTemplate {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures/sse")
        .join(format!("{name}.txt"));
    let body = std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
}

/// Plays responses in order; a request past the end gets a 500.
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

/// A home whose daemon keeps secrets in a file and talks to `mentor`,
/// and a workspace with the files the recorded reads ask for.
struct Home {
    dir: tempfile::TempDir,
    workspace: tempfile::TempDir,
}

impl Home {
    fn new(mentor: &MockServer) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            format!(
                "[daemon]\nsecret_store = \"file\"\nidle_shutdown_min = 0\n\n[mentor]\nbase_url = \"{}\"\nmax_retries = 0\ntimeout_s = 30\n",
                mentor.uri()
            ),
        )
        .unwrap();
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
        Self { dir, workspace }
    }

    fn harness(&self) -> assert_cmd::Command {
        let mut c = assert_cmd::Command::new(harness_bin());
        c.arg("--home")
            .arg(self.dir.path())
            .env_remove("HARNESS_LOG_LEVEL")
            .env("ANTHROPIC_API_KEY", API_KEY)
            .env("HARNESS_DAEMON_PATH", HARNESSD)
            .env("NO_COLOR", "1")
            .timeout(Duration::from_secs(60));
        c
    }

    fn start(&self) {
        self.harness()
            .args(["--json", "daemon", "start"])
            .assert()
            .success();
    }

    fn stop(&self) {
        self.harness().args(["daemon", "stop"]).assert().success();
        let data = self.dir.path().join("data");
        let deadline = Instant::now() + Duration::from_secs(10);
        while DaemonInfo::read(&data).unwrap().is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            DaemonInfo::read(&data).unwrap().is_none(),
            "daemon still up"
        );
    }

    /// Every event of `session` in order, from `trace list`.
    fn events(&self, session: &str) -> Vec<Value> {
        let out = self
            .harness()
            .args([
                "--json",
                "trace",
                "list",
                "--session",
                session,
                "--limit",
                "200",
            ])
            .assert()
            .success();
        json(&out)["events"].as_array().unwrap().clone()
    }

    fn event(&self, id: &str) -> Value {
        json(
            &self
                .harness()
                .args(["--json", "trace", "show", id])
                .assert()
                .success(),
        )["event"]
            .clone()
    }
}

fn json(out: &Assert) -> Value {
    serde_json::from_slice(&out.get_output().stdout).unwrap()
}

fn kinds(events: &[Value]) -> Vec<&str> {
    events.iter().map(|e| e["kind"].as_str().unwrap()).collect()
}

/// Runs `f` and stops the daemon even when `f` panics.
fn with_daemon(home: &Home, f: impl FnOnce()) {
    home.start();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    home.stop();
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_three_step_run_lands_in_the_trace_and_the_session_continues() {
    let mentor = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Script(Mutex::new(
            vec![
                sse("tool_use_parallel"),
                sse("tool_use_write"),
                sse("text"),
                sse("text"),
            ]
            .into(),
        )))
        .mount(&mentor)
        .await;
    let h = Arc::new(Home::new(&mentor));
    let hh = Arc::clone(&h);
    tokio::task::spawn_blocking(move || {
        let h = hh;
        with_daemon(&h, || {
            let out = h
                .harness()
                .arg("--json")
                .arg("run")
                .arg("--workspace")
                .arg(h.workspace.path())
                .args(["--permission-mode", "auto", "add hello"])
                .assert()
                .success();
            let stdout = String::from_utf8_lossy(&out.get_output().stdout);
            let docs: Vec<Value> = stdout
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            let types: Vec<&str> = docs.iter().map(|d| d["type"].as_str().unwrap()).collect();
            let count = |t: &str| types.iter().filter(|x| **x == t).count();
            assert_eq!(types[0], "agent.started", "{stdout}");
            assert_eq!(types[1], "agent.step");
            assert_eq!(count("agent.step"), 5, "3 mentor phases + 2 tool phases");
            assert_eq!(count("agent.tool_call"), 3);
            assert_eq!(count("agent.tool_result"), 3);
            assert_eq!(count("agent.usage"), 3);
            assert_eq!(count("permission.decision"), 3);
            assert_eq!(count("agent.finished"), 1);
            assert_eq!(&types[types.len() - 2..], ["agent.finished", "result"]);
            let steps: Vec<(u64, &str)> = docs
                .iter()
                .filter(|d| d["type"] == "agent.step")
                .map(|d| (d["seq"].as_u64().unwrap(), d["phase"].as_str().unwrap()))
                .collect();
            assert_eq!(
                steps,
                [
                    (1, "mentor"),
                    (1, "tools"),
                    (2, "mentor"),
                    (2, "tools"),
                    (3, "mentor")
                ]
            );
            let result = docs.last().unwrap();
            assert_eq!(result["status"], "ok");
            assert_eq!(result["usage"]["input_tokens"], 412 + 650 + 25);
            let session = result["session_id"].as_str().unwrap().to_owned();
            assert!(h.workspace.path().join("src/hello.rs").is_file());

            // The trace, as the CLI lists it: three steps in order.
            let events = h.events(&session);
            let kinds = kinds(&events);
            let count = |k: &str| kinds.iter().filter(|x| **x == k).count();
            assert_eq!(count("mentor.request"), 3, "{kinds:?}");
            assert_eq!(count("mentor.response"), 3);
            assert_eq!(count("tool.call"), 3);
            assert_eq!(count("tool.result"), 3);
            assert_eq!(count("permission.decision"), 3);
            assert_eq!(count("workspace.snapshot"), 2);
            assert_eq!(count("outcome"), 1);
            assert_eq!(
                kinds[..4],
                [
                    "session.created",
                    "agent.started",
                    "user.message",
                    "workspace.snapshot"
                ]
            );
            assert_eq!(
                kinds[kinds.len() - 3..],
                ["workspace.snapshot", "outcome", "agent.finished"]
            );
            let step_ids: Vec<&str> = events
                .iter()
                .filter(|e| e["kind"] == "mentor.request")
                .map(|e| e["step_id"].as_str().unwrap())
                .collect();
            assert_eq!(step_ids.len(), 3);
            assert!(step_ids[0] != step_ids[1] && step_ids[1] != step_ids[2]);
            let tool_steps: Vec<&str> = events
                .iter()
                .filter(|e| e["kind"] == "tool.call")
                .map(|e| e["step_id"].as_str().unwrap())
                .collect();
            assert_eq!(tool_steps, [step_ids[0], step_ids[0], step_ids[1]]);
            let outcome = events.iter().find(|e| e["kind"] == "outcome").unwrap();
            let outcome = h.event(outcome["id"].as_str().unwrap());
            assert_eq!(outcome["payload"]["kind"], "files_changed");
            assert_eq!(
                outcome["payload"]["details"]["added"],
                json!(["src/hello.rs"])
            );

            // The same session, continued: the fourth call carries the
            // whole history.
            h.harness()
                .args(["run", "--session", &session, "thanks"])
                .assert()
                .success()
                .stdout("Hello, world!\n");
        });
    })
    .await
    .unwrap();
    let received = mentor.received_requests().await.unwrap();
    assert_eq!(received.len(), 4);
    let last: Value = serde_json::from_slice(&received[3].body).unwrap();
    let messages = last["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 7);
    assert_eq!(messages[6]["content"][0]["text"], "thanks");
    assert_eq!(messages[2]["content"].as_array().unwrap().len(), 2);
    assert_eq!(messages[4]["content"][0]["tool_use_id"], "toolu_01C");
    let bodies: Vec<Value> = received
        .iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert!(
        bodies.iter().all(|b| b["tools"] == bodies[0]["tools"]),
        "tool set changed"
    );
    let _ = h;
}
