//! Session persistence end to end (task M01-10): the real `harness` CLI
//! against the real `harnessd` with a wiremock mentor. A run is cut by
//! `daemon stop` while the third call is in flight (after two steps of
//! tool results); the next daemon resumes the session from the store
//! and the mentor gets the whole history plus the new prompt. Then the
//! `session` commands: list, show, search, rename, export, `run
//! --last`, archive, delete.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// Plays responses in order and counts the completion requests; one
/// past the end gets a 500.
struct Script {
    responses: Mutex<VecDeque<ResponseTemplate>>,
    seen: Arc<AtomicUsize>,
}

impl Respond for Script {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        self.seen.fetch_add(1, Ordering::SeqCst);
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| {
                ResponseTemplate::new(500).set_body_json(json!({
                    "type": "error",
                    "error": {"type": "api_error", "message": "script exhausted"}
                }))
            })
    }
}

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
                "[daemon]\nsecret_store = \"file\"\nidle_shutdown_min = 0\n\n[sessions]\nauto_title = false\n\n[mentor]\nbase_url = \"{}\"\nmax_retries = 0\ntimeout_s = 60\n",
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
            .timeout(Duration::from_secs(90));
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
        let deadline = Instant::now() + Duration::from_secs(15);
        while DaemonInfo::read(&data).unwrap().is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            DaemonInfo::read(&data).unwrap().is_none(),
            "daemon still up"
        );
    }

    /// `harness --json session <args>` parsed.
    fn session_json(&self, args: &[&str]) -> Value {
        let mut c = self.harness();
        c.args(["--json", "session"]).args(args);
        json(&c.assert().success())
    }
}

fn json(out: &Assert) -> Value {
    serde_json::from_slice(&out.get_output().stdout).unwrap()
}

fn stdout(out: &Assert) -> String {
    String::from_utf8_lossy(&out.get_output().stdout).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_cut_by_a_daemon_stop_resumes_and_the_session_commands_work() {
    let mentor = MockServer::start().await;
    let seen = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Script {
            responses: Mutex::new(
                vec![
                    sse("tool_use_parallel"),
                    sse("tool_use_write"),
                    // The third call never answers in time: the daemon
                    // is stopped while it waits.
                    sse("text").set_delay(Duration::from_secs(40)),
                    sse("text"),
                    sse("text"),
                ]
                .into(),
            ),
            seen: Arc::clone(&seen),
        })
        .mount(&mentor)
        .await;
    let h = Arc::new(Home::new(&mentor));
    let hh = Arc::clone(&h);
    let seen_by_thread = Arc::clone(&seen);
    tokio::task::spawn_blocking(move || {
        let h = hh;
        let seen = seen_by_thread;
        h.start();
        let runner = {
            let h = Arc::clone(&h);
            std::thread::spawn(move || {
                let out = h
                    .harness()
                    .arg("--json")
                    .arg("run")
                    .arg("--workspace")
                    .arg(h.workspace.path())
                    .args(["--permission-mode", "auto", "add hello"])
                    .output()
                    .unwrap();
                String::from_utf8_lossy(&out.stdout).into_owned()
            })
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        while seen.load(Ordering::SeqCst) < 3 {
            assert!(Instant::now() < deadline, "the third call never came");
            std::thread::sleep(Duration::from_millis(50));
        }
        // Two steps of tool results are stored; the third call is in
        // flight. Stop the daemon under it.
        h.stop();
        let first_run = runner.join().unwrap();
        let session = first_run
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|d| d["type"] == "agent.started")
            .map(|d| d["session_id"].as_str().unwrap().to_owned())
            .expect("agent.started in the first run's output");

        h.start();
        let list = h.session_json(&["list", "--workspace", &h.workspace.path().to_string_lossy()]);
        let sessions = list["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1, "{list}");
        assert_eq!(sessions[0]["id"], session);
        assert_eq!(sessions[0]["message_count"], 5);
        assert_eq!(sessions[0]["last_agent_status"], "cancelled");
        assert_eq!(sessions[0]["title"], "add hello");
        assert_eq!(sessions[0]["status"], "open");

        // The resumed run: the whole history goes out, plus the prompt.
        let out = h
            .harness()
            .args(["--json", "run", "--session", &session, "continue"])
            .assert()
            .success();
        let result: Value = stdout(&out)
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .find(|d: &Value| d["type"] == "result")
            .unwrap();
        assert_eq!(result["status"], "ok");
        assert_eq!(result["session_id"], session);
        let plain = h
            .harness()
            .args(["run", "--session", &session, "and again"])
            .assert()
            .success();
        assert_eq!(stdout(&plain), "Hello, world!\n");

        // The conversation, as the CLI shows it.
        let shown = h.session_json(&["show", &session]);
        let roles: Vec<&str> = shown["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(
            roles,
            [
                "user",
                "assistant",
                "user",
                "assistant",
                "user",
                "assistant",
                "user",
                "assistant"
            ]
        );
        assert_eq!(shown["session"]["prompt_version"], "mentor_system_v1");
        assert_eq!(shown["session"]["last_agent_status"], "ok");
        let text = stdout(
            &h.harness()
                .args(["session", "show", &session])
                .assert()
                .success(),
        );
        assert!(text.contains("--- user #1 ("), "{text}");
        assert!(text.contains("add hello"), "{text}");
        assert!(text.contains("→ write_file "), "{text}");
        assert!(text.contains("Hello, world!"), "{text}");

        // Search, rename, export.
        let hits = h.session_json(&["search", "hello world"]);
        assert!(!hits["hits"].as_array().unwrap().is_empty(), "{hits}");
        assert_eq!(hits["hits"][0]["session_id"], session);
        assert!(
            hits["hits"][0]["snippet"]
                .as_str()
                .unwrap()
                .contains("[Hello]"),
            "{hits}"
        );
        h.harness()
            .args(["session", "rename", &session, "Resume test"])
            .assert()
            .success();
        let list = h.session_json(&["list"]);
        assert_eq!(list["sessions"][0]["title"], "Resume test");
        let export = h.dir.path().join("session.json");
        h.harness()
            .args(["session", "export", &session, "-o"])
            .arg(&export)
            .assert()
            .success();
        let exported: Value = serde_json::from_slice(&std::fs::read(&export).unwrap()).unwrap();
        assert_eq!(exported["format"], "harness-session/1");
        assert_eq!(exported["messages"].as_array().unwrap().len(), 8);
        assert_eq!(exported["session"]["title"], "Resume test");
        assert_eq!(exported["session"]["title_source"], "user");
        assert_eq!(exported["mentor_calls"].as_array().unwrap().len(), 5);

        // `--last` picks the workspace's most recent session.
        let resumed_last = h
            .harness()
            .arg("--json")
            .arg("run")
            .arg("--last")
            .arg("--workspace")
            .arg(h.workspace.path())
            .arg("once more")
            .assert()
            .failure();
        let result: Value = stdout(&resumed_last)
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .find(|d: &Value| d["type"] == "result")
            .unwrap();
        assert_eq!(
            result["session_id"], session,
            "the script is exhausted, the session is the point"
        );
        assert_eq!(h.session_json(&["list"])["sessions"][0]["message_count"], 9);

        // Archive hides; delete with purge removes everything.
        h.harness()
            .args(["session", "archive", &session])
            .assert()
            .success();
        assert!(
            h.session_json(&["list"])["sessions"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let all = h.session_json(&["list", "--all"]);
        assert_eq!(all["sessions"][0]["status"], "archived");
        let deleted = h.session_json(&["delete", &session, "--purge-traces"]);
        assert_eq!(deleted["messages_deleted"], 9);
        assert!(
            deleted["events_deleted"].as_u64().unwrap() > 20,
            "{deleted}"
        );
        assert!(
            h.session_json(&["list", "--all"])["sessions"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        h.harness()
            .args(["session", "show", &session])
            .assert()
            .failure();
        h.stop();
    })
    .await
    .unwrap();

    // The mentor saw the resumed history: five stored messages, the
    // prompt joined to the last user turn, the same tools as before.
    let received = mentor.received_requests().await.unwrap();
    let bodies: Vec<Value> = received
        .iter()
        .filter(|r| r.url.path() == "/v1/messages")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect();
    assert!(bodies.len() >= 5, "{}", bodies.len());
    let resumed = &bodies[3];
    let messages = resumed["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 5);
    assert_eq!(messages[4]["role"], "user");
    assert_eq!(messages[4]["content"][0]["type"], "tool_result");
    assert_eq!(messages[4]["content"][0]["tool_use_id"], "toolu_01C");
    assert_eq!(messages[4]["content"][1]["text"], "continue");
    assert_eq!(resumed["tools"], bodies[0]["tools"]);
    // The core prompt is the same bytes; the workspace block is built
    // afresh by the new daemon (the tree changed: `src/hello.rs`).
    assert_eq!(resumed["system"][0], bodies[0]["system"][0]);
    assert!(
        resumed["system"][1]["text"]
            .as_str()
            .unwrap()
            .contains("src/hello.rs")
            || resumed["system"][1]["text"]
                .as_str()
                .unwrap()
                .contains("src/, Cargo.toml"),
        "{}",
        resumed["system"][1]
    );
    let _ = h;
}
