//! The M00 exit criterion (task M00-11): the real `harness` CLI against
//! the real `harnessd`, whose mentor is a wiremock server replaying
//! recorded SSE. A round trip lands in the trace store byte for byte;
//! cancellation, an API error, concurrent runs and a graceful shutdown
//! mid-run each leave the record they should.
//!
//! `harness` is found next to `harnessd`, or built with `cargo build -p
//! harness` when missing.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use apprentice_client::DaemonInfo;
use assert_cmd::assert::Assert;
use predicates::prelude::*;
use serde_json::Value;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

fn fixture(name: &str) -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures/sse")
        .join(format!("{name}.txt"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn sse(name: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(fixture(name), "text/event-stream")
}

/// A home whose daemon keeps secrets in a file and talks to `mentor`.
struct Home {
    dir: tempfile::TempDir,
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
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn data(&self) -> PathBuf {
        self.path().join("data")
    }

    /// A CLI invocation against this home. The daemon it may spawn
    /// inherits the API key from the environment.
    fn harness(&self) -> assert_cmd::Command {
        let mut c = assert_cmd::Command::new(harness_bin());
        c.arg("--home")
            .arg(self.path())
            .env_remove("HARNESS_LOG_LEVEL")
            .env("ANTHROPIC_API_KEY", API_KEY)
            .env("HARNESS_DAEMON_PATH", HARNESSD)
            .env("NO_COLOR", "1")
            .timeout(Duration::from_secs(60));
        c
    }

    /// A `harness run` as a raw child in its own process group, so an
    /// interrupt reaches it alone.
    fn run_child(&self, prompt: &str) -> std::process::Child {
        let mut cmd = Command::new(harness_bin());
        cmd.arg("--home")
            .arg(self.path())
            .args(["run", prompt])
            .env_remove("HARNESS_LOG_LEVEL")
            .env("ANTHROPIC_API_KEY", API_KEY)
            .env("HARNESS_DAEMON_PATH", HARNESSD)
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }
        cmd.spawn().unwrap()
    }

    fn start(&self) -> u32 {
        let out = self
            .harness()
            .args(["--json", "daemon", "start"])
            .assert()
            .success();
        u32::try_from(json(&out)["pid"].as_u64().unwrap()).unwrap()
    }

    fn stop(&self) {
        self.harness().args(["daemon", "stop"]).assert().success();
        let data = self.data();
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
                "100",
            ])
            .assert()
            .success();
        json(&out)["events"].as_array().unwrap().clone()
    }

    fn event(&self, id: &str, blob: bool) -> Value {
        let mut args = vec!["--json", "trace", "show", id];
        if blob {
            args.push("--blob");
        }
        json(&self.harness().args(args).assert().success())
    }

    fn daemon_log(&self) -> String {
        let logs = self.data().join("logs");
        let mut out = String::new();
        for entry in std::fs::read_dir(logs).unwrap() {
            let p = entry.unwrap().path();
            if p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("daemon.")
            {
                out.push_str(&std::fs::read_to_string(p).unwrap());
            }
        }
        out
    }
}

fn json(out: &Assert) -> Value {
    serde_json::from_slice(&out.get_output().stdout).unwrap()
}

fn kinds(events: &[Value]) -> Vec<&str> {
    events.iter().map(|e| e["kind"].as_str().unwrap()).collect()
}

fn find<'a>(events: &'a [Value], kind: &str) -> &'a Value {
    events
        .iter()
        .find(|e| e["kind"] == kind)
        .unwrap_or_else(|| panic!("no {kind} in {:?}", kinds(events)))
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
async fn hello_round_trips_through_the_mentor_into_the_trace() {
    let mentor = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", API_KEY))
        .respond_with(sse("text"))
        .mount(&mentor)
        .await;
    let h = Arc::new(Home::new(&mentor));
    let inspect = tokio::task::spawn_blocking(move || {
        // spawn_blocking so the mock server's runtime keeps serving.
        let mut result = None;
        with_daemon(&h, || {
            let out = h
                .harness()
                .args(["--json", "run", "hello"])
                .assert()
                .success();
            let stdout = String::from_utf8_lossy(&out.get_output().stdout);
            let docs: Vec<Value> = stdout
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect();
            let types: Vec<&str> = docs.iter().map(|d| d["type"].as_str().unwrap()).collect();
            assert_eq!(
                types,
                [
                    "agent.started",
                    "agent.text_delta",
                    "agent.text_delta",
                    "agent.text_delta",
                    "agent.text_delta",
                    "agent.usage",
                    "agent.finished",
                    "result"
                ],
                "{stdout}"
            );
            let result_doc = docs.last().unwrap();
            assert_eq!(result_doc["status"], "ok");
            assert_eq!(result_doc["usage"]["input_tokens"], 25);
            assert_eq!(result_doc["usage"]["output_tokens"], 12);
            assert_eq!(result_doc["cost_usd"], 0.000_425);
            let session = result_doc["session_id"].as_str().unwrap().to_owned();
            let agent = result_doc["agent_id"].as_str().unwrap().to_owned();

            // Plain output: the text on stdout, the usage line on stderr.
            h.harness()
                .args(["run", "hello"])
                .assert()
                .success()
                .stdout("Hello, world!\n")
                .stderr(predicate::str::contains(
                    "in 25 · out 12 · cache read 0 · $0.0004 ·",
                ));

            let events = h.events(&session);
            assert_eq!(
                kinds(&events),
                [
                    "session.created",
                    "agent.started",
                    "user.message",
                    "mentor.request",
                    "mentor.response",
                    "assistant.message",
                    "agent.finished",
                ]
            );
            let seqs: Vec<u64> = events.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
            assert_eq!(seqs, [1, 2, 3, 4, 5, 6, 7]);
            assert!(events[1..].iter().all(|e| e["agent_id"] == agent));
            let request_id = find(&events, "mentor.request")["id"].as_str().unwrap();
            let request = h.event(request_id, true);
            let finished = find(&events, "agent.finished");
            assert_eq!(
                h.event(finished["id"].as_str().unwrap(), false)["event"]["payload"]["status"],
                "ok"
            );
            result = Some((session, request));
        });
        result.unwrap()
    })
    .await
    .unwrap();
    let (session, request) = inspect;

    // The stored request body is exactly what the server received (for
    // the first of the two runs; both bodies are identical).
    let received = mentor.received_requests().await.unwrap();
    assert_eq!(received.len(), 2);
    assert_eq!(received[0].body, received[1].body);
    let blob = request["blob"].as_str().expect("blob text");
    assert_eq!(blob.as_bytes(), received[0].body.as_slice());
    let body: Value = serde_json::from_slice(&received[0].body).unwrap();
    assert_eq!(body["model"], "claude-opus-5");
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(body["messages"][0]["content"][0]["text"], "hello");
    assert_eq!(body["thinking"]["type"], "adaptive");
    assert_eq!(body["output_config"]["effort"], "high");
    assert_eq!(request["event"]["payload"]["bytes"], received[0].body.len());
    assert_eq!(request["event"]["payload"]["model"], "claude-opus-5");
    assert!(session.len() > 20);
}

#[tokio::test(flavor = "multi_thread")]
async fn usage_and_cost_show_up_in_stats() {
    let mentor = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse("text"))
        .mount(&mentor)
        .await;
    let h = Arc::new(Home::new(&mentor));
    tokio::task::spawn_blocking(move || {
        with_daemon(&h, || {
            h.harness().args(["run", "hello"]).assert().success();
            let stats = json(
                &h.harness()
                    .args(["--json", "stats", "tokens"])
                    .assert()
                    .success(),
            );
            assert_eq!(stats["totals"]["calls"], 1);
            assert_eq!(stats["totals"]["input"], 25);
            assert_eq!(stats["totals"]["output"], 12);
            assert_eq!(stats["totals"]["cost_usd"], 0.000_425);
            assert_eq!(stats["by_model"][0]["key"], "claude-opus-5");
        });
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_cancels_the_run_and_the_trace_says_so() {
    let mentor = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse("text").set_delay(Duration::from_secs(30)))
        .mount(&mentor)
        .await;
    let h = Arc::new(Home::new(&mentor));
    tokio::task::spawn_blocking(move || {
        with_daemon(&h, || {
            let child = h.run_child("slow");
            let pid = child.id();
            // Let the run reach the mentor.
            std::thread::sleep(Duration::from_millis(1500));
            if !interrupt(pid) {
                eprintln!("skipped: cannot deliver an interrupt from this process");
                let mut child = child;
                child.kill().unwrap();
                child.wait().unwrap();
                return;
            }
            let out = child.wait_with_output().unwrap();
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(out.status.code(), Some(130), "{stderr}");
            assert!(stderr.contains("cancelling"), "{stderr}");
            assert!(stderr.contains("agent cancelled"), "{stderr}");
            let session = stderr
                .lines()
                .find_map(|l| l.strip_prefix("session "))
                .expect("session line")
                .trim()
                .to_owned();
            let events = h.events(&session);
            assert_eq!(
                kinds(&events),
                [
                    "session.created",
                    "agent.started",
                    "user.message",
                    "mentor.request",
                    "mentor.error",
                    "agent.finished",
                ]
            );
            let err = h.event(find(&events, "mentor.error")["id"].as_str().unwrap(), false);
            assert_eq!(err["event"]["payload"]["kind"], "cancelled");
            let done = h.event(
                find(&events, "agent.finished")["id"].as_str().unwrap(),
                false,
            );
            assert_eq!(done["event"]["payload"]["status"], "cancelled");
        });
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_authentication_error_is_exit_2_and_recorded() {
    let mentor = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_string(
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
        ))
        .mount(&mentor)
        .await;
    let h = Arc::new(Home::new(&mentor));
    tokio::task::spawn_blocking(move || {
        with_daemon(&h, || {
            let out = h
                .harness()
                .args(["--json", "run", "hello"])
                .assert()
                .code(2)
                .stderr(predicate::str::contains("error: agent failed: "))
                .stderr(predicate::str::contains("authentication"))
                .stderr(predicate::str::contains("[mentor_error]"));
            let stdout = String::from_utf8_lossy(&out.get_output().stdout);
            let last: Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
            assert_eq!(last["type"], "result");
            assert_eq!(last["status"], "error");
            assert_eq!(last["error"]["data"]["kind"], "mentor_error");
            assert_eq!(last["error"]["data"]["details"]["http_status"], 401);
            let session = last["session_id"].as_str().unwrap();
            let events = h.events(session);
            assert_eq!(
                kinds(&events),
                [
                    "session.created",
                    "agent.started",
                    "user.message",
                    "mentor.request",
                    "mentor.error",
                    "agent.finished",
                ]
            );
            let done = h.event(
                find(&events, "agent.finished")["id"].as_str().unwrap(),
                false,
            );
            assert_eq!(done["event"]["payload"]["status"], "error");
            assert_eq!(
                done["event"]["payload"]["error"]["data"]["kind"],
                "mentor_error"
            );
        });
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn two_concurrent_runs_are_two_agents_with_their_own_traces() {
    let mentor = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse("text").set_delay(Duration::from_millis(500)))
        .mount(&mentor)
        .await;
    let h = Arc::new(Home::new(&mentor));
    tokio::task::spawn_blocking(move || {
        with_daemon(&h, || {
            let a = h.run_child("first");
            let b = h.run_child("second");
            let a = a.wait_with_output().unwrap();
            let b = b.wait_with_output().unwrap();
            for out in [&a, &b] {
                let stderr = String::from_utf8_lossy(&out.stderr);
                assert!(out.status.success(), "{stderr}");
                assert_eq!(String::from_utf8_lossy(&out.stdout), "Hello, world!\n");
            }
            let session_of = |out: &std::process::Output| {
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .find_map(|l| l.strip_prefix("session ").map(str::to_owned))
                    .expect("session line")
            };
            let (sa, sb) = (session_of(&a), session_of(&b));
            assert_ne!(sa, sb);
            let mut agents = Vec::new();
            for (session, prompt) in [(&sa, "first"), (&sb, "second")] {
                let events = h.events(session);
                assert_eq!(events.len(), 7, "{:?}", kinds(&events));
                let seqs: Vec<u64> = events.iter().map(|e| e["seq"].as_u64().unwrap()).collect();
                assert_eq!(seqs, [1, 2, 3, 4, 5, 6, 7]);
                let agent = events[1]["agent_id"].as_str().unwrap().to_owned();
                assert!(events[1..].iter().all(|e| e["agent_id"] == agent));
                let started = h.event(events[1]["id"].as_str().unwrap(), false);
                assert_eq!(started["event"]["payload"]["task_text"], prompt);
                agents.push(agent);
            }
            assert_ne!(agents[0], agents[1]);
        });
    })
    .await
    .unwrap();
    assert_eq!(mentor.received_requests().await.unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_graceful_shutdown_cancels_the_run_and_records_it() {
    let mentor = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse("text").set_delay(Duration::from_secs(30)))
        .mount(&mentor)
        .await;
    let h = Arc::new(Home::new(&mentor));
    tokio::task::spawn_blocking(move || {
        h.start();
        let child = h.run_child("slow");
        std::thread::sleep(Duration::from_millis(1500));
        let stopped = Instant::now();
        h.stop();
        let out = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stopped.elapsed() < Duration::from_secs(8), "{stderr}");
        // The client saw the cancellation before the daemon closed.
        assert_eq!(out.status.code(), Some(130), "{stderr}");
        assert!(stderr.contains("agent cancelled"), "{stderr}");
        let session = stderr
            .lines()
            .find_map(|l| l.strip_prefix("session ").map(str::to_owned))
            .expect("session line");
        let log = h.daemon_log();
        assert!(log.contains("\"stopped\""), "{log}");
        assert!(!log.contains("still running at shutdown"), "{log}");

        // A fresh daemon reads the record the old one flushed.
        with_daemon(&h, || {
            let events = h.events(&session);
            assert_eq!(
                kinds(&events).last().copied(),
                Some("agent.finished"),
                "{:?}",
                kinds(&events)
            );
            let done = h.event(
                find(&events, "agent.finished")["id"].as_str().unwrap(),
                false,
            );
            assert_eq!(done["event"]["payload"]["status"], "cancelled");
            let err = h.event(find(&events, "mentor.error")["id"].as_str().unwrap(), false);
            assert_eq!(err["event"]["payload"]["kind"], "cancelled");
        });
    })
    .await
    .unwrap();
}

/// Delivers CTRL-C-equivalent to `pid`: `CTRL_BREAK` to its process group
/// on Windows (the CLI treats both alike), SIGINT elsewhere.
#[cfg(windows)]
fn interrupt(pid: u32) -> bool {
    #[allow(
        unsafe_code,
        reason = "GenerateConsoleCtrlEvent has no safe wrapper; a plain FFI call with no pointers"
    )]
    fn send(pid: u32) -> bool {
        use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
        // SAFETY: both arguments are plain integers; the call touches no
        // memory owned by this process.
        unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) != 0 }
    }
    send(pid)
}

#[cfg(unix)]
fn interrupt(pid: u32) -> bool {
    Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()
        .is_ok_and(|s| s.success())
}
