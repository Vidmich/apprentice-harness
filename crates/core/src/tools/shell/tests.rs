//! The behaviours the task pins down: exit code, duration and streams
//! of a plain command; timeouts and cancellation killing the whole
//! tree; big output capped and streamed without stalling; the scrubbed
//! environment; background jobs. Commands come in a PowerShell and a
//! POSIX form and the assertions are the same on every platform.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::config::{ShellConfig, ToolsConfig};
use crate::tools::{SeenFiles, ToolContent, ToolEnv, ToolProgress, ToolValidator};
use crate::trace::{AgentId, SessionId};
use crate::workspace::Workspace;

struct Fixture {
    dir: tempfile::TempDir,
    ws: Arc<Workspace>,
    config: Arc<ToolsConfig>,
    shell: Arc<dyn Tool>,
    jobs: Arc<dyn Tool>,
    env: ToolEnv,
}

impl Fixture {
    fn new() -> Self {
        Self::with_config(ToolsConfig::default())
    }

    fn with_config(config: ToolsConfig) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        let ws = Arc::new(Workspace::open(dir.path()).unwrap());
        let mut tools = shell_tools();
        let _run_tests = tools.pop().unwrap();
        let jobs = tools.pop().unwrap();
        let shell = tools.pop().unwrap();
        Self {
            dir,
            ws,
            config: Arc::new(config),
            shell,
            jobs,
            env: ToolEnv::default(),
        }
    }

    fn ctx(&self, progress: Option<mpsc::Sender<ToolProgress>>) -> ToolContext {
        let progress = progress.unwrap_or_else(|| mpsc::channel(1).0);
        ToolContext {
            workspace: Some(Arc::clone(&self.ws)),
            session_id: SessionId::generate(),
            agent_id: AgentId::generate(),
            call_id: "t1".into(),
            env: self.env.clone(),
            config: Arc::clone(&self.config),
            seen: Arc::new(SeenFiles::new()),
            progress,
        }
    }

    async fn call_with(
        &self,
        tool: &Arc<dyn Tool>,
        ctx: &ToolContext,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        ToolValidator::compile(&tool.spec().input_schema)
            .unwrap()
            .validate(&input)?;
        tool.call(ctx, input, cancel).await
    }

    async fn shell(&self, input: Value) -> Result<ToolOutput, ToolError> {
        self.call_with(
            &self.shell,
            &self.ctx(None),
            input,
            CancellationToken::new(),
        )
        .await
    }

    async fn run(&self, command: &str) -> ToolOutput {
        self.shell(json!({"command": command})).await.unwrap()
    }

    async fn jobs(&self, input: Value) -> ToolOutput {
        self.call_with(&self.jobs, &self.ctx(None), input, CancellationToken::new())
            .await
            .unwrap()
    }

    fn path(&self, rel: &str) -> std::path::PathBuf {
        self.dir.path().join(rel)
    }
}

/// The command for this platform.
fn cmd(pwsh: &str, posix: &str) -> String {
    if cfg!(windows) { pwsh } else { posix }.to_owned()
}

fn text(out: &ToolOutput) -> String {
    match &out.content {
        ToolContent::Text(t) => t.replace("\r\n", "\n"),
        other => panic!("not text: {other:?}"),
    }
}

fn attachment(out: &ToolOutput, name: &str) -> Option<String> {
    out.attachments
        .iter()
        .find(|a| a.name == name)
        .map(|a| String::from_utf8_lossy(&a.bytes).replace("\r\n", "\n"))
}

fn footer(out: &ToolOutput) -> String {
    text(out).lines().last().unwrap_or_default().to_owned()
}

/// Reads the pid a command wrote to `file`, waiting for it to appear.
async fn pid_from(file: &Path) -> u32 {
    for _ in 0..100 {
        if let Ok(s) = std::fs::read_to_string(file)
            && let Ok(pid) = s.trim().parse()
        {
            return pid;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no pid in {}", file.display());
}

/// Is a process with `pid` still around?
fn alive(pid: u32) -> bool {
    if cfg!(windows) {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
    } else {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .is_ok_and(|o| o.status.success())
    }
}

/// `alive(pid)` turning false within `within`.
async fn gone(pid: u32, within: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < within {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    !alive(pid)
}

#[tokio::test]
async fn echo_hi_reports_the_exit_code_the_duration_and_the_streams() {
    let f = Fixture::new();
    let out = f.run("echo hi").await;
    assert!(!out.is_error);
    assert_eq!(text(&out).lines().next(), Some("hi"));
    let footer = footer(&out);
    assert!(footer.starts_with("exit 0 · "), "{footer}");
    assert!(
        footer.ends_with(" ms") || footer.ends_with(" s"),
        "{footer}"
    );
    assert!(out.summary.starts_with("exit 0 in "), "{}", out.summary);
    assert_eq!(out.metadata["exit_code"], 0);
    assert_eq!(out.metadata["stdout_lines"], 1);
    assert_eq!(out.metadata["stderr_bytes"], 0);
    assert_eq!(out.metadata["timed_out"], false);
    assert_eq!(out.metadata["truncated"], false);
    assert!(out.metadata["duration_ms"].as_u64().is_some());
    assert!(
        out.metadata["shell"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
    );
    assert_eq!(attachment(&out, "stdout").as_deref(), Some("hi\n"));
    assert_eq!(attachment(&out, "stderr"), None);
}

#[tokio::test]
async fn stderr_is_tagged_and_a_nonzero_exit_is_not_an_error() {
    let f = Fixture::new();
    let out = f
        .run(&cmd(
            "[Console]::Error.WriteLine('oops'); echo out; exit 3",
            "echo oops >&2; echo out; exit 3",
        ))
        .await;
    assert!(!out.is_error, "{}", text(&out));
    let text = text(&out);
    assert!(text.contains("[err]\noops\n"), "{text}");
    assert!(text.contains("out\n"), "{text}");
    assert!(footer(&out).starts_with("exit 3 · "), "{text}");
    assert_eq!(out.metadata["exit_code"], 3);
    assert_eq!(out.metadata["stdout_lines"], 1);
    assert_eq!(out.metadata["stderr_lines"], 1);
    assert_eq!(attachment(&out, "stdout").as_deref(), Some("out\n"));
    assert_eq!(attachment(&out, "stderr").as_deref(), Some("oops\n"));
    assert!(out.summary.starts_with("exit 3 in "), "{}", out.summary);
}

#[tokio::test]
async fn silence_leaves_just_the_footer() {
    let f = Fixture::new();
    let out = f.run(&cmd("exit 0", "true")).await;
    assert!(text(&out).starts_with("exit 0 · "), "{}", text(&out));
    assert_eq!(text(&out).lines().count(), 1);
    assert!(out.attachments.is_empty());
}

#[tokio::test]
async fn cwd_is_relative_to_the_workspace_and_stays_inside_it() {
    let f = Fixture::new();
    let out = f
        .shell(json!({"command": cmd("(Get-Location).Path", "pwd"), "cwd": "sub"}))
        .await
        .unwrap();
    let first = text(&out).lines().next().unwrap().to_owned();
    assert!(
        Path::new(&first).file_name().is_some_and(|n| n == "sub"),
        "{first}"
    );

    let out = f
        .shell(json!({"command": "echo hi", "cwd": "missing"}))
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(text(&out).contains("not a directory"), "{}", text(&out));

    let err = f
        .shell(json!({"command": "echo hi", "cwd": "../.."}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied(_)), "{err:?}");

    let err = f.shell(json!({"command": ""})).await.unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)), "{err:?}");
}

#[tokio::test]
async fn the_timeout_kills_the_command_and_its_children() {
    let f = Fixture::new();
    let pid_file = f.path("pid.txt");
    // A child of the child, sleeping.
    let command = cmd(
        "pwsh -NoProfile -Command '$pid | Set-Content -Path pid.txt; Start-Sleep 60'",
        "sh -c 'echo $$ > pid.txt; exec sleep 60'",
    );
    let started = Instant::now();
    let out = f
        .shell(json!({"command": command, "timeout_s": 3}))
        .await
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{:?}",
        started.elapsed()
    );
    assert!(out.is_error);
    assert_eq!(footer(&out), "timed out after 3 s; process tree killed");
    assert!(
        out.summary.starts_with("timed out after 3 s"),
        "{}",
        out.summary
    );
    assert_eq!(out.metadata["timed_out"], true);
    assert_eq!(out.metadata["exit_code"], Value::Null);
    let grandchild = pid_from(&pid_file).await;
    assert!(
        gone(grandchild, Duration::from_secs(2)).await,
        "grandchild {grandchild} still alive"
    );
}

#[tokio::test]
async fn cancelling_the_agent_kills_the_tree_within_a_second() {
    let f = Fixture::new();
    let pid_file = f.path("pid.txt");
    let command = cmd(
        "$pid | Set-Content -Path pid.txt; Start-Sleep 60",
        "echo $$ > pid.txt; sleep 60",
    );
    let cancel = CancellationToken::new();
    let ctx = f.ctx(None);
    let call = f.call_with(&f.shell, &ctx, json!({"command": command}), cancel.clone());
    let canceller = async {
        let pid = pid_from(&pid_file).await;
        cancel.cancel();
        (pid, Instant::now())
    };
    let (result, (pid, cancelled_at)) = tokio::join!(call, canceller);
    let took = cancelled_at.elapsed();
    assert!(matches!(result, Err(ToolError::Cancelled)), "{result:?}");
    assert!(took < Duration::from_secs(1), "{took:?}");
    assert!(gone(pid, Duration::from_secs(1)).await, "{pid} still alive");
}

#[tokio::test]
async fn big_output_is_capped_and_streamed_without_stalling() {
    let mut f = Fixture::new();
    f.env.max_capture_bytes = 1024 * 1024;
    // 20 MiB: 20 480 lines of 1023 x's.
    let command = cmd(
        "$s = 'x' * 1023; foreach ($i in 1..20480) { $s }",
        "awk 'BEGIN { s = sprintf(\"%1023s\", \"\"); gsub(/ /, \"x\", s); for (i = 0; i < 20480; i++) print s }'",
    );
    let (tx, mut rx) = mpsc::channel(4);
    let consumer = tokio::spawn(async move {
        let mut got = Vec::new();
        while let Some(p) = rx.recv().await {
            got.push(p);
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        got
    });
    let ctx = f.ctx(Some(tx));
    let started = Instant::now();
    let out = f
        .call_with(
            &f.shell,
            &ctx,
            json!({"command": command, "timeout_s": 120}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let took = started.elapsed();
    drop(ctx);
    let got = consumer.await.unwrap();

    assert!(!out.is_error, "{}", footer(&out));
    assert_eq!(out.metadata["stdout_lines"], 20_480);
    assert_eq!(out.metadata["truncated"], true);
    let text = text(&out);
    assert!(text.len() <= 1024 * 1024 + 64, "{}", text.len());
    assert!(text.starts_with("xxxxxxxx"), "{}", &text[..40]);
    assert!(text.contains("\n[... "), "no omission marker");
    assert!(text.contains(" bytes omitted]\n"), "no omission marker");
    assert!(footer(&out).starts_with("exit 0 · "));
    let stdout = attachment(&out, "stdout").unwrap();
    assert!(stdout.len() <= 1024 * 1024 + 64);
    assert!(
        stdout.contains(" bytes of stdout omitted]\n"),
        "no stdout marker"
    );

    // The consumer took 40 ms per chunk and never held the command up.
    assert!(took < Duration::from_secs(60), "{took:?}");
    let dropped = out.metadata["progress_dropped_lines"].as_u64().unwrap_or(0);
    assert!(
        dropped > 0,
        "nothing dropped: {} chunks delivered",
        got.len()
    );
    assert!(
        got.iter()
            .any(|p| p.text.starts_with("[... ")
                && p.text.ends_with(" lines of output not shown]\n")),
        "no lag marker in {} chunks",
        got.len()
    );
    assert!(
        got.iter()
            .all(|p| p.call_id == "t1" && p.stream == ProgressStream::Stdout)
    );
}

#[tokio::test]
async fn scrubbed_variables_are_gone_and_the_hints_are_set() {
    // Cargo sets `CARGO_PKG_NAME` for every test binary.
    let f = Fixture::with_config(ToolsConfig {
        shell: ShellConfig {
            scrub_env: vec!["CARGO_PKG_*".into()],
            env: [("CI".to_owned(), "1".to_owned())].into(),
            ..ShellConfig::default()
        },
        ..ToolsConfig::default()
    });
    let out = f
        .run(&cmd(
            "\"[$env:CARGO_PKG_NAME] [$env:HARNESS] [$env:NO_COLOR] [$env:TERM] [$env:CI]\"",
            "echo \"[$CARGO_PKG_NAME] [$HARNESS] [$NO_COLOR] [$TERM] [$CI]\"",
        ))
        .await;
    assert_eq!(text(&out).lines().next(), Some("[] [1] [1] [dumb] [1]"));
}

#[tokio::test]
async fn background_jobs_start_poll_wait_and_kill() {
    let f = Fixture::new();
    let ticker = cmd(
        "foreach ($i in 1..100) { \"tick $i\"; Start-Sleep -Milliseconds 100 }",
        "i=0; while [ $i -lt 100 ]; do i=$((i+1)); echo tick $i; sleep 0.1; done",
    );
    let out = f
        .shell(json!({"command": ticker, "background": true, "description": "ticks"}))
        .await
        .unwrap();
    assert!(!out.is_error);
    assert_eq!(out.metadata["job_id"], "j1");
    assert_eq!(out.metadata["background"], true);
    assert_eq!(out.summary, "started job j1");
    assert!(
        text(&out).starts_with("started job j1 (pid "),
        "{}",
        text(&out)
    );

    let list = f.jobs(json!({"action": "list"})).await;
    let line = text(&list).lines().next().unwrap().to_owned();
    assert!(line.starts_with("j1\trunning · "), "{line}");
    assert!(
        line.ends_with("\tforeach ($i in 1..100) { \"tick $i\"; Start-Sleep -Milliseconds 100 }")
            || line.ends_with("; done"),
        "{line}"
    );
    assert_eq!(text(&list).lines().last(), Some("[1 jobs, 1 running]"));

    // Output so far, while it runs.
    let mut seen_tick = false;
    for _ in 0..50 {
        let out = f.jobs(json!({"action": "output", "job_id": "j1"})).await;
        assert_eq!(out.metadata["running"], true);
        if text(&out).contains("tick 1\n") {
            seen_tick = true;
            assert!(footer(&out).starts_with("running · "), "{}", footer(&out));
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(seen_tick);

    // `wait` gives up after its timeout while the job runs on.
    let out = f
        .jobs(json!({"action": "wait", "job_id": "j1", "timeout_s": 1}))
        .await;
    assert_eq!(out.metadata["running"], true);

    let out = f.jobs(json!({"action": "kill", "job_id": "j1"})).await;
    assert_eq!(out.metadata["running"], false);
    assert_eq!(out.metadata["killed"], true);
    assert!(footer(&out).starts_with("killed · "), "{}", footer(&out));
    assert!(text(&out).contains("tick 1\n"));
    assert!(
        out.summary.starts_with("j1: killed after "),
        "{}",
        out.summary
    );
    let list = f.jobs(json!({"action": "list"})).await;
    assert!(
        text(&list)
            .lines()
            .next()
            .unwrap()
            .starts_with("j1\tkilled · "),
        "{}",
        text(&list)
    );
    assert_eq!(text(&list).lines().last(), Some("[1 jobs, 0 running]"));

    // A job that ends on its own: `wait` returns its final output.
    let out = f
        .shell(
            json!({"command": cmd("echo done; exit 4", "echo done; exit 4"), "background": true}),
        )
        .await
        .unwrap();
    assert_eq!(out.metadata["job_id"], "j2");
    let out = f
        .jobs(json!({"action": "wait", "job_id": "j2", "timeout_s": 30}))
        .await;
    assert_eq!(out.metadata["running"], false);
    assert_eq!(out.metadata["exit_code"], 4);
    assert_eq!(text(&out).lines().next(), Some("done"));
    assert!(footer(&out).starts_with("exit 4 · "), "{}", footer(&out));
    assert!(out.summary.starts_with("j2: exit 4 in "), "{}", out.summary);

    let out = f.jobs(json!({"action": "output", "job_id": "j9"})).await;
    assert!(out.is_error);
    assert_eq!(text(&out), "no job `j9`; known jobs: j1, j2");
    let err = f
        .call_with(
            &f.jobs,
            &f.ctx(None),
            json!({"action": "kill"}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput(_)), "{err:?}");
}

#[tokio::test]
async fn a_background_job_dies_with_the_agent_unless_detached() {
    let f = Fixture::new();
    let command = cmd(
        "$pid | Set-Content -Path pid.txt; Start-Sleep 60",
        "echo $$ > pid.txt; sleep 60",
    );
    let agent = CancellationToken::new();
    let out = f
        .call_with(
            &f.shell,
            &f.ctx(None),
            json!({"command": command, "background": true}),
            agent.child_token(),
        )
        .await
        .unwrap();
    let pid = pid_from(&f.path("pid.txt")).await;
    assert_eq!(out.metadata["pid"], pid);
    assert!(alive(pid));
    agent.cancel();
    assert!(
        gone(pid, Duration::from_secs(2)).await,
        "{pid} survived the agent"
    );
    let out = f
        .jobs(json!({"action": "wait", "job_id": "j1", "timeout_s": 5}))
        .await;
    assert_eq!(out.metadata["killed"], true);

    // Detached: the agent's token means nothing; `kill` still works.
    std::fs::remove_file(f.path("pid.txt")).unwrap();
    let agent = CancellationToken::new();
    let command = cmd(
        "$pid | Set-Content -Path pid.txt; Start-Sleep 60",
        "echo $$ > pid.txt; sleep 60",
    );
    f.call_with(
        &f.shell,
        &f.ctx(None),
        json!({"command": command, "background": true, "detach": true}),
        agent.child_token(),
    )
    .await
    .unwrap();
    let pid = pid_from(&f.path("pid.txt")).await;
    agent.cancel();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(alive(pid), "{pid} died with the agent although detached");
    let out = f.jobs(json!({"action": "kill", "job_id": "j2"})).await;
    assert_eq!(out.metadata["killed"], true);
    assert!(
        gone(pid, Duration::from_secs(2)).await,
        "{pid} survived the kill"
    );
}

#[test]
fn durations_read_well() {
    assert_eq!(fmt_duration(Duration::from_millis(12)), "12 ms");
    assert_eq!(fmt_duration(Duration::from_millis(3_240)), "3.2 s");
    assert_eq!(fmt_duration(Duration::from_secs(125)), "2 min 5 s");
}

#[test]
fn shell_tool_specs_golden() {
    let specs: Vec<ToolSpec> = shell_tools().iter().map(|t| t.spec()).collect();
    insta::assert_json_snapshot!("shell_tool_specs", specs);
}
