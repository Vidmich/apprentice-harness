//! `harness run` against a fake daemon on a real local socket (task
//! M00-09): streamed text, the usage line, `--json` NDJSON, an agent
//! error, CTRL-C → `agent.cancel` → exit 130, and the permission prompt
//! answered from stdin (task M01-07). The real `agent.run` arrives with
//! M00-11; this router only plays the protocol.

use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use apprentice_api::events::{AgentStatus, Event, Risk};
use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::{
    AgentCancel, AgentRun, AgentRunParams, AgentRunResult, Empty, PermissionRespond,
    PermissionRespondParams, SessionCreate, SessionCreateResult,
};
use apprentice_api::server::{Router, RouterConfig};
use apprentice_api::transport::Endpoint;
use apprentice_api::types::{
    Effort, PermissionAnswer, PermissionDecision, PermissionMode, PermissionSource, RuleEffect,
    RuleMatch, RuleSpec, RunOptions, Usage,
};
use apprentice_client::DaemonInfo;
use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tokio::sync::Notify;

fn harness(home: &Path) -> Command {
    let mut c = Command::cargo_bin("harness").unwrap();
    c.arg("--home")
        .arg(home)
        .env_remove("HARNESS_LOG_LEVEL")
        .env("NO_COLOR", "1");
    c
}

/// What the fake daemon saw.
#[derive(Default)]
struct Seen {
    runs: Vec<AgentRunParams>,
    cancels: Vec<String>,
    sessions_created: usize,
    answers: Vec<PermissionRespondParams>,
}

/// Serves a router on `<home>/data` and writes its `daemon.json`. The
/// prompt picks the script: `fail` ends in an error, `slow` streams for
/// ten seconds unless cancelled, `ask` (or `ask ask`, for two prompts)
/// asks permission for a shell command and reports the answer, anything
/// else says hello.
fn fake_daemon(home: &Path) -> (Arc<Mutex<Seen>>, tokio::task::JoinHandle<()>) {
    let data = home.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let cancel = Arc::new(Notify::new());
    let answered = Arc::new(Notify::new());

    let mut router = Router::new(RouterConfig {
        daemon_version: "9.9.9".into(),
        pid: std::process::id(),
        token: Some("secret".into()),
    });
    router.add::<PermissionRespond, _, _>({
        let seen = Arc::clone(&seen);
        let answered = Arc::clone(&answered);
        move |_c, p: PermissionRespondParams| {
            let seen = Arc::clone(&seen);
            let answered = Arc::clone(&answered);
            async move {
                if p.request_id.starts_with("req-") {
                    seen.lock().unwrap().answers.push(p);
                    answered.notify_one();
                    Ok(Empty {})
                } else {
                    Err(RpcError::not_found("no such request"))
                }
            }
        }
    });

    router.add::<SessionCreate, _, _>({
        let seen = Arc::clone(&seen);
        move |_c, _p| {
            let seen = Arc::clone(&seen);
            async move {
                seen.lock().unwrap().sessions_created += 1;
                Ok(SessionCreateResult {
                    session_id: "sess-1".into(),
                })
            }
        }
    });
    router.add::<AgentCancel, _, _>({
        let seen = Arc::clone(&seen);
        let cancel = Arc::clone(&cancel);
        move |_c, p| {
            let seen = Arc::clone(&seen);
            let cancel = Arc::clone(&cancel);
            async move {
                seen.lock().unwrap().cancels.push(p.agent_id);
                cancel.notify_one();
                Ok(Empty {})
            }
        }
    });
    router.add::<AgentRun, _, _>({
        let seen = Arc::clone(&seen);
        let cancel = Arc::clone(&cancel);
        let answered = Arc::clone(&answered);
        move |conn, p| {
            let seen = Arc::clone(&seen);
            let cancel = Arc::clone(&cancel);
            let answered = Arc::clone(&answered);
            async move {
                let script = p.prompt.clone();
                seen.lock().unwrap().runs.push(p);
                let agent_id = "agent-1".to_owned();
                let sub = agent_id.clone();
                tokio::spawn(async move {
                    let a = agent_id.clone();
                    let started = Event::AgentStarted {
                        agent_id: a.clone(),
                        session_id: "sess-1".into(),
                    };
                    let _ = conn.notify(&sub, started).await;
                    let (status, error) = match script.as_str() {
                        s if s.starts_with("ask") => {
                            for (i, _) in s.split_whitespace().enumerate() {
                                let request_id = format!("req-{}", i + 1);
                                let ask = Event::PermissionRequest {
                                    request_id: request_id.clone(),
                                    agent_id: a.clone(),
                                    tool: "shell".into(),
                                    input: serde_json::json!({"command": "cargo test -p core"}),
                                    risk: Risk::Execute,
                                    description: "shell: cargo test -p core".into(),
                                    command: Some("cargo test -p core".into()),
                                    paths: vec![".".into()],
                                    suggested_rules: vec![RuleSpec {
                                        tool: "shell".into(),
                                        effect: RuleEffect::Allow,
                                        r#match: RuleMatch {
                                            command_prefix: Some("cargo test".into()),
                                            ..RuleMatch::default()
                                        },
                                    }],
                                    timeout_s: 600,
                                };
                                let _ = conn.notify(&sub, ask).await;
                                let answer = tokio::select! {
                                    () = answered.notified() => {
                                        seen.lock().unwrap().answers.last().map(|p| p.answer)
                                    }
                                    () = tokio::time::sleep(Duration::from_secs(10)) => None,
                                };
                                let allowed = matches!(
                                    answer,
                                    Some(
                                        PermissionAnswer::AllowOnce
                                            | PermissionAnswer::AllowSession
                                            | PermissionAnswer::AllowWorkspace
                                            | PermissionAnswer::AllowAlways
                                    )
                                );
                                let decision = Event::PermissionDecision {
                                    agent_id: a.clone(),
                                    call_id: format!("c{}", i + 1),
                                    tool: "shell".into(),
                                    decision: if allowed {
                                        PermissionDecision::Allow
                                    } else {
                                        PermissionDecision::Deny
                                    },
                                    source: if answer.is_some() {
                                        PermissionSource::User
                                    } else {
                                        PermissionSource::Timeout
                                    },
                                    request_id: Some(request_id),
                                    rule_ref: None,
                                    reason: (!allowed).then(|| "denied by user".to_owned()),
                                };
                                let _ = conn.notify(&sub, decision).await;
                                let delta = Event::AgentTextDelta {
                                    agent_id: a.clone(),
                                    text: format!(
                                        "{}\n",
                                        if allowed { "ran it" } else { "did not run it" }
                                    ),
                                };
                                let _ = conn.notify(&sub, delta).await;
                            }
                            // A denial by rule, without a prompt.
                            let by_rule = Event::PermissionDecision {
                                agent_id: a.clone(),
                                call_id: "c9".into(),
                                tool: "write_file".into(),
                                decision: PermissionDecision::Deny,
                                source: PermissionSource::Rule,
                                request_id: None,
                                rule_ref: Some("workspace:1".into()),
                                reason: Some("denied by rule workspace:1 (write_file [path=secrets/**])".into()),
                            };
                            let _ = conn.notify(&sub, by_rule).await;
                            (AgentStatus::Ok, None)
                        }
                        "fail" => (
                            AgentStatus::Error,
                            Some(
                                RpcError::new(-32020, "mentor_error", "mentor rejected the request")
                                    .with_details(serde_json::json!({
                                        "status": 401, "type": "authentication_error"
                                    })),
                            ),
                        ),
                        "slow" => {
                            let mut status = AgentStatus::Ok;
                            for i in 0..100 {
                                let delta = Event::AgentTextDelta {
                                    agent_id: a.clone(),
                                    text: format!("tick {i}\n"),
                                };
                                let _ = conn.notify(&sub, delta).await;
                                tokio::select! {
                                    () = tokio::time::sleep(Duration::from_millis(100)) => {}
                                    () = cancel.notified() => { status = AgentStatus::Cancelled; break; }
                                }
                            }
                            (status, None)
                        }
                        _ => {
                            for text in ["Hello, ", "world!"] {
                                let delta = Event::AgentTextDelta {
                                    agent_id: a.clone(),
                                    text: text.into(),
                                };
                                let _ = conn.notify(&sub, delta).await;
                            }
                            let usage = Event::AgentUsage {
                                agent_id: a.clone(),
                                call_id: "m1".into(),
                                usage: Usage {
                                    input_tokens: 1204,
                                    output_tokens: 310,
                                    cache_read_input_tokens: 0,
                                    cache_creation_input_tokens: 0,
                                },
                                cost_usd: Some(0.0138),
                                session_usage: Usage {
                                    input_tokens: 1204,
                                    output_tokens: 310,
                                    cache_read_input_tokens: 0,
                                    cache_creation_input_tokens: 0,
                                },
                                session_cost_usd: Some(0.0138),
                            };
                            let _ = conn.notify(&sub, usage).await;
                            (AgentStatus::Ok, None)
                        }
                    };
                    let finished = Event::AgentFinished {
                        agent_id: a,
                        status,
                        error,
                        truncated: false,
                    };
                    let _ = conn.notify(&sub, finished).await;
                });
                Ok(AgentRunResult {
                    agent_id: "agent-1".into(),
                    subscription: "agent-1".into(),
                })
            }
        }
    });

    let router = Arc::new(router);
    let endpoint = Endpoint::default_for(
        &data,
        &format!("cli-run-{}-{}", std::process::id(), home.display()),
    );
    let listener = endpoint.listen().unwrap();
    let serve = tokio::spawn(async move {
        loop {
            let (r, w) = listener.accept().await.unwrap();
            let router = Arc::clone(&router);
            tokio::spawn(async move {
                let _ = router.serve(r, w).await;
            });
        }
    });
    let info = DaemonInfo {
        pid: std::process::id(),
        endpoint,
        token: "secret".into(),
        api_version: apprentice_api::API_VERSION,
        version: "9.9.9".into(),
        started_at: "2026-01-01T00:00:00Z".into(),
    };
    info.write(&data).unwrap();
    (seen, serve)
}

#[tokio::test(flavor = "multi_thread")]
async fn streams_text_and_prints_the_usage_line() {
    let home = tempfile::tempdir().unwrap();
    let (seen, serve) = fake_daemon(home.path());
    let home_path = home.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        harness(&home_path)
            .args(["run", "say hi"])
            .assert()
            .success()
            .stdout("Hello, world!\n")
            .stderr(predicate::str::contains("session sess-1\n"))
            .stderr(predicate::str::contains(
                "↳ in 1,204 · out 310 · cache read 0 · $0.0138 · ",
            ));

        // Options reach the daemon; `--session` skips session.create.
        harness(&home_path)
            .args([
                "run",
                "--session",
                "given",
                "--model",
                "m-x",
                "--effort",
                "high",
                "--no-apprentice",
                "--quiet",
                "again",
            ])
            .assert()
            .success()
            .stdout("Hello, world!\n")
            .stderr("");
    })
    .await
    .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.sessions_created, 1);
    assert_eq!(seen.runs.len(), 2);
    assert_eq!(seen.runs[0].session_id, "sess-1");
    assert_eq!(seen.runs[0].options, RunOptions::default());
    let second = &seen.runs[1];
    assert_eq!(second.session_id, "given");
    assert_eq!(second.options.model.as_deref(), Some("m-x"));
    assert_eq!(second.options.effort, Some(Effort::High));
    assert_eq!(second.options.apprentice, Some(false));
    serve.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn json_is_ndjson_ending_in_a_result() {
    let home = tempfile::tempdir().unwrap();
    let (_seen, serve) = fake_daemon(home.path());
    let home_path = home.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let out = harness(&home_path)
            .args(["--json", "run", "say hi"])
            .assert()
            .success();
        let stdout = String::from_utf8_lossy(&out.get_output().stdout);
        let docs: Vec<Value> = stdout
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .collect();
        let types: Vec<&str> = docs.iter().map(|d| d["type"].as_str().unwrap()).collect();
        assert_eq!(
            types,
            [
                "agent.started",
                "agent.text_delta",
                "agent.text_delta",
                "agent.usage",
                "agent.finished",
                "result"
            ]
        );
        let result = docs.last().unwrap();
        assert_eq!(result["status"], "ok");
        assert_eq!(result["agent_id"], "agent-1");
        assert_eq!(result["session_id"], "sess-1");
        assert_eq!(result["usage"]["input_tokens"], 1204);
        assert_eq!(result["cost_usd"], 0.0138);
        assert!(result["elapsed_s"].as_f64().unwrap() >= 0.0);

        // An agent error: exit 2, error in the result and on stderr.
        let out = harness(&home_path)
            .args(["--json", "run", "fail"])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(
                "error: agent failed: mentor rejected the request [mentor_error] (401 authentication_error)",
            ));
        let stdout = String::from_utf8_lossy(&out.get_output().stdout);
        let last: Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
        assert_eq!(last["type"], "result");
        assert_eq!(last["status"], "error");
        assert_eq!(last["error"]["data"]["kind"], "mentor_error");

        harness(&home_path)
            .args(["run", "fail"])
            .assert()
            .code(2)
            .stdout("")
            .stderr(predicate::str::contains("[mentor_error]"));
    })
    .await
    .unwrap();
    serve.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn permission_prompts_are_answered_from_stdin() {
    let home = tempfile::tempdir().unwrap();
    let (seen, serve) = fake_daemon(home.path());
    let home_path = home.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        // One prompt, answered `a`; the prompt shows the command, the
        // risk and the rule `w`/`A` would write.
        harness(&home_path)
            .args(["run", "--permission-mode", "auto", "ask"])
            .write_stdin("a\n")
            .assert()
            .success()
            .stdout("ran it\n")
            .stderr(predicate::str::contains(
                "? shell: cargo test -p core (execute)\n  rule for w/A: allow shell [command_prefix=\"cargo test\"]\n  (denied in 600 s without an answer)\n  [a]llow once / [s]ession / [w]orkspace / [A]lways / [d]eny / [D]eny always: ",
            ))
            .stderr(predicate::str::contains("  shell: allowed once\n"))
            .stderr(predicate::str::contains(
                "✗ write_file: denied by rule workspace:1 (write_file [path=secrets/**])\n",
            ));

        // Two prompts: a bad answer is asked again, `D` denies always;
        // EOF on the second denies once.
        harness(&home_path)
            .args(["run", "ask ask"])
            .write_stdin("x\nD\n")
            .assert()
            .success()
            .stdout("did not run it\ndid not run it\n")
            .stderr(predicate::str::contains("answer with one of a, s, w, A, d, D"))
            .stderr(predicate::str::contains("shell: always denied (rule written)"))
            .stderr(predicate::str::contains("shell: denied\n"));

        // `--json`: no prompt, denied at once, the events still stream.
        let out = harness(&home_path)
            .args(["--json", "run", "ask"])
            .write_stdin("a\n")
            .assert()
            .success()
            .stderr(predicate::str::contains(
                "permission for shell: cargo test -p core: denied (--json)",
            ));
        let stdout = String::from_utf8_lossy(&out.get_output().stdout);
        let types: Vec<String> = stdout
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap()["type"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            types,
            [
                "agent.started",
                "permission.request",
                "permission.decision",
                "agent.text_delta",
                "permission.decision",
                "agent.finished",
                "result"
            ]
        );
    })
    .await
    .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen.runs[0].options.permission_mode,
        Some(PermissionMode::Auto)
    );
    assert_eq!(seen.runs[1].options.permission_mode, None);
    let answers: Vec<(&str, PermissionAnswer)> = seen
        .answers
        .iter()
        .map(|p| (p.request_id.as_str(), p.answer))
        .collect();
    assert_eq!(
        answers,
        [
            ("req-1", PermissionAnswer::AllowOnce),
            ("req-1", PermissionAnswer::DenyAlways),
            ("req-2", PermissionAnswer::DenyOnce),
            ("req-1", PermissionAnswer::DenyOnce),
        ]
    );
    assert!(seen.answers.iter().all(|p| p.rule.is_none()));
    serve.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_cancels_the_agent_and_exits_130() {
    let home = tempfile::tempdir().unwrap();
    let (seen, serve) = fake_daemon(home.path());
    let home_path = home.path().to_path_buf();
    let outcome = tokio::task::spawn_blocking(move || {
        let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin("harness"));
        cmd.arg("--home")
            .arg(&home_path)
            .args(["run", "slow"])
            .env_remove("HARNESS_LOG_LEVEL")
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
        let child = cmd.spawn().unwrap();
        let pid = child.id();
        // Let a few ticks through first.
        std::thread::sleep(Duration::from_millis(1500));
        if !interrupt(pid) {
            eprintln!("skipped: cannot deliver an interrupt from this process");
            let mut child = child;
            child.kill().unwrap();
            child.wait().unwrap();
            return None;
        }
        Some(child.wait_with_output().unwrap())
    })
    .await
    .unwrap();
    let Some(out) = outcome else {
        serve.abort();
        return;
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(130), "{stdout}\n{stderr}");
    assert!(stdout.starts_with("tick 0\ntick 1\n"), "{stdout}");
    assert!(stdout.lines().count() < 100, "{stdout}");
    assert!(stderr.contains("cancelling"), "{stderr}");
    assert!(stderr.contains("agent cancelled"), "{stderr}");
    assert!(stderr.contains("↳ in 0 · out 0"), "{stderr}");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.cancels, ["agent-1"]);
    serve.abort();
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
    std::process::Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()
        .is_ok_and(|s| s.success())
}
