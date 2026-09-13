//! The shell tools (task M01-05): `shell` runs a command in the
//! workspace, `shell_jobs` looks after the ones started in the
//! background.
//!
//! A command runs through PowerShell (`pwsh`, else `powershell`) on
//! Windows and through `$SHELL -lc` (else `/bin/sh`) elsewhere —
//! `tools.shell.program` / `args` override that — in a scrubbed copy
//! of the daemon's environment, inside a process group / Job Object so
//! that a timeout, a cancellation or a kill takes the whole tree down.
//! Output streams to the call's progress channel as it arrives and is
//! captured in full up to `tools.max_capture_bytes` per stream (head
//! and tail past that). The mentor reads the combined transcript, in
//! arrival order with `[err]` / `[out]` lines where the stream changes,
//! and a footer `exit <code> · <duration>`; stdout and stderr are
//! stored separately as attachments of the `tool.result` event. A
//! non-zero exit is an ordinary result — the footer carries the code —
//! but a timeout is an error result.
//!
//! Never `cmd.exe`: its quoting is unreliable, and the mentor writes
//! POSIX-ish commands more consistently for PowerShell 7.

mod capture;
mod jobs;
mod process;
mod program;
#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use self::capture::{Reporter, Transcript};
use self::jobs::{Job, Jobs, NewJob};
use self::process::{Ended, Outcome, spawn};
use self::program::Program;
use super::file::{parse, target};
use super::{ProgressStream, Risk, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};

/// `timeout_s` when the mentor gives none.
pub const SHELL_DEFAULT_TIMEOUT_S: u64 = 120;
/// Longest `timeout_s` the schema accepts (config may lower it).
pub const SHELL_MAX_TIMEOUT_S: u64 = 3600;
/// Longest `shell_jobs wait`.
pub const JOBS_MAX_WAIT_S: u64 = 600;
/// The wrapper's own timeout for `shell`, past the longest run plus
/// the kill grace, so the tool always gets to return the output.
const WRAPPER_TIMEOUT: Duration = Duration::from_secs(SHELL_MAX_TIMEOUT_S + 60);
/// How long `kill` waits for the job's final status.
const KILL_WAIT: Duration = Duration::from_secs(3);

/// The two shell tools, sharing one job registry.
pub fn shell_tools() -> Vec<Arc<dyn Tool>> {
    let jobs = Arc::new(Jobs::default());
    vec![
        Arc::new(Shell {
            jobs: Arc::clone(&jobs),
        }),
        Arc::new(ShellJobs { jobs }),
    ]
}

const SHELL_DESCRIPTION: &str = "Runs a command in the workspace and returns its output.

The command goes to PowerShell (`pwsh -NoProfile -NonInteractive -Command`, or `powershell` where pwsh is missing) on Windows and to the login shell (`$SHELL -lc`, else `/bin/sh`) elsewhere. It runs in `cwd` (a directory relative to the workspace root, default the root) with stdin closed, `NO_COLOR=1` and `TERM=dumb` set and secrets removed from the environment.

The result is the combined output in the order it appeared — stdout as is, a `[err]` line before a run of stderr lines and `[out]` where stdout resumes — followed by `exit <code> · <duration>`. A non-zero exit is reported the same way: read the output and the code; it is not a failure of the tool. Long output is cut in the middle (`[... N bytes omitted]`). A command still running after `timeout_s` seconds (default 120, at most 3600) is killed with its whole process tree and the output so far comes back with `timed out after N s`.

Use `background: true` for servers, watchers and anything that should keep running: the call returns a job id at once and `shell_jobs` reads the job's output, waits for it or kills it. Background jobs die with the agent unless `detach: true`.

Prefer the dedicated tools for reading, writing, listing and searching files: they are faster and their output is smaller.";

const JOBS_DESCRIPTION: &str = "Looks after the background jobs started with `shell` (`background: true`).

`action`: `list` — every job with its status; `output` — the output of `job_id` so far, ending in its status line (`running · 12.3 s` or `exit <code> · <duration>`); `wait` — like `output`, but first waits up to `timeout_s` seconds (default 30) for the job to end; `kill` — kills the job's process tree and returns its final output.";

/// `shell`.
#[derive(Debug)]
pub struct Shell {
    jobs: Arc<Jobs>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    command: String,
    cwd: Option<String>,
    #[serde(default = "default_timeout")]
    timeout_s: u64,
    #[serde(default)]
    background: bool,
    #[serde(default)]
    detach: bool,
    description: Option<String>,
}

fn default_timeout() -> u64 {
    SHELL_DEFAULT_TIMEOUT_S
}

#[async_trait]
impl Tool for Shell {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "shell",
            SHELL_DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "minLength": 1,
                        "description": "The command line, as typed at the shell's prompt."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Directory to run in, relative to the workspace root (default: the root)."
                    },
                    "timeout_s": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": SHELL_MAX_TIMEOUT_S,
                        "default": SHELL_DEFAULT_TIMEOUT_S,
                        "description": "Seconds before the command and its process tree are killed."
                    },
                    "background": {
                        "type": "boolean",
                        "default": false,
                        "description": "Return at once with a job id; manage the job with `shell_jobs`."
                    },
                    "detach": {
                        "type": "boolean",
                        "default": false,
                        "description": "With `background`: let the job outlive the agent."
                    },
                    "description": {
                        "type": "string",
                        "maxLength": 200,
                        "description": "One line for the user saying what the command is for."
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
            Risk::Execute,
        )
        .with_tags(["shell"])
        .with_timeout(WRAPPER_TIMEOUT)
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let input: Input = parse(input)?;
        let (_, t) = target(ctx, input.cwd.as_deref().unwrap_or("."))?;
        if !t.abs.is_dir() {
            return Ok(ToolOutput::error(format!(
                "`{}` is not a directory in the workspace",
                t.shown
            )));
        }
        let config = &ctx.config.shell;
        let program = Program::resolve(config);
        let timeout = Duration::from_secs(
            input
                .timeout_s
                .min(config.max_timeout_s)
                .clamp(1, SHELL_MAX_TIMEOUT_S),
        );
        let detached = input.background && input.detach;
        let spawned = match spawn(&program, config, &input.command, &t.abs, !detached) {
            Ok(s) => s,
            Err(e) => {
                return Ok(ToolOutput::error(format!(
                    "cannot start `{}`: {e}",
                    program.path.display()
                )));
            }
        };

        if input.background {
            let job = self.jobs.start(
                spawned,
                NewJob {
                    command: input.command.clone(),
                    description: input.description.clone(),
                    detached,
                    agent_id: ctx.agent_id.clone(),
                    timeout,
                    cancel,
                    max_bytes: ctx.env.max_capture_bytes,
                },
            );
            return Ok(ToolOutput::text(format!(
                "started job {} (pid {}) in {}: {}\nread its output with shell_jobs {{\"action\": \"output\", \"job_id\": \"{}\"}}\n",
                job.id, job.pid, t.shown, input.command, job.id
            ))
            .with_summary(format!("started job {}", job.id))
            .with_metadata(json!({
                "job_id": job.id,
                "pid": job.pid,
                "background": true,
                "detached": detached,
                "shell": program.name,
                "description": input.description,
            })));
        }

        let mut transcript = Transcript::new(ctx.env.max_capture_bytes);
        let mut reporter = Reporter::new(ctx);
        let outcome = process::pump(spawned, timeout, &cancel, |stream, bytes| {
            transcript.push(stream, bytes);
            reporter.push(stream, String::from_utf8_lossy(bytes).into_owned());
        })
        .await;
        reporter.flush();
        if outcome.ended == Ended::Killed {
            return Err(ToolError::Cancelled);
        }
        let mut out = render(&transcript, &outcome, timeout);
        out.metadata["shell"] = json!(program.name);
        out.metadata["description"] = json!(input.description);
        if reporter.dropped_lines() > 0 {
            out.metadata["progress_dropped_lines"] = json!(reporter.dropped_lines());
        }
        Ok(out)
    }
}

/// The result of a finished run (foreground or job): transcript, notes,
/// footer; metadata; the streams as attachments.
fn render(transcript: &Transcript, outcome: &Outcome, timeout: Duration) -> ToolOutput {
    let mut text = transcript.text();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if outcome.orphans_killed {
        text.push_str("[processes left running by the command were killed]\n");
    }
    let end = Ending::of(outcome, timeout);
    text.push_str(&end.footer);
    text.push('\n');
    let metadata = json!({
        "exit_code": end.exit_code,
        "signal": end.signal,
        "duration_ms": u64::try_from(outcome.duration.as_millis()).unwrap_or(u64::MAX),
        "timed_out": outcome.ended == Ended::TimedOut,
        "killed": outcome.ended == Ended::Killed,
        "stdout_bytes": transcript.stdout().total(),
        "stderr_bytes": transcript.stderr().total(),
        "stdout_lines": transcript.stdout().lines(),
        "stderr_lines": transcript.stderr().lines(),
        "truncated": transcript.truncated(),
        "orphans_killed": outcome.orphans_killed,
    });
    let mut out = if end.is_error {
        ToolOutput::error(text)
    } else {
        ToolOutput::text(text)
    };
    out = out.with_summary(end.summary).with_metadata(metadata);
    if let Some(bytes) = transcript.stream_bytes(ProgressStream::Stdout) {
        out = out.with_text_attachment("stdout", bytes);
    }
    if let Some(bytes) = transcript.stream_bytes(ProgressStream::Stderr) {
        out = out.with_text_attachment("stderr", bytes);
    }
    out
}

/// The footer and summary of a finished run.
struct Ending {
    footer: String,
    summary: String,
    exit_code: Option<i32>,
    signal: Option<i32>,
    is_error: bool,
}

impl Ending {
    fn of(outcome: &Outcome, timeout: Duration) -> Self {
        let dur = fmt_duration(outcome.duration);
        let plain = |footer: String, summary: String| Self {
            footer,
            summary,
            exit_code: None,
            signal: None,
            is_error: false,
        };
        match outcome.ended {
            Ended::Exited(status) => match (status.code(), exit_signal(status)) {
                (Some(code), _) => Self {
                    exit_code: Some(code),
                    ..plain(
                        format!("exit {code} · {dur}"),
                        format!("exit {code} in {dur}"),
                    )
                },
                (None, Some(sig)) => Self {
                    signal: Some(sig),
                    ..plain(
                        format!("killed by signal {sig} · {dur}"),
                        format!("killed by signal {sig} in {dur}"),
                    )
                },
                (None, None) => plain(
                    format!("ended without a status · {dur}"),
                    format!("ended without a status in {dur}"),
                ),
            },
            Ended::TimedOut => {
                let secs = timeout.as_secs();
                Self {
                    is_error: true,
                    ..plain(
                        format!("timed out after {secs} s; process tree killed"),
                        format!("timed out after {secs} s"),
                    )
                }
            }
            Ended::Killed => plain(format!("killed · {dur}"), format!("killed after {dur}")),
        }
    }
}

#[cfg(unix)]
fn exit_signal(status: std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_: std::process::ExitStatus) -> Option<i32> {
    None
}

/// `12 ms`, `3.2 s`, `2 min 5 s`.
fn fmt_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 1.0 {
        format!("{} ms", d.as_millis())
    } else if secs < 60.0 {
        format!("{secs:.1} s")
    } else {
        let whole = d.as_secs();
        format!("{} min {} s", whole / 60, whole % 60)
    }
}

/// `shell_jobs`.
#[derive(Debug)]
pub struct ShellJobs {
    jobs: Arc<Jobs>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Action {
    List,
    Output,
    Wait,
    Kill,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobsInput {
    action: Action,
    job_id: Option<String>,
    #[serde(default = "default_wait")]
    timeout_s: u64,
}

fn default_wait() -> u64 {
    30
}

#[async_trait]
impl Tool for ShellJobs {
    fn spec(&self) -> ToolSpec {
        // Read-only: it only observes or stops what the agent itself
        // started (and was permitted to) through `shell`.
        ToolSpec::new(
            "shell_jobs",
            JOBS_DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["list", "output", "wait", "kill"]
                    },
                    "job_id": {
                        "type": "string",
                        "description": "The job (`j1`, `j2`, ...); required except for `list`."
                    },
                    "timeout_s": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": JOBS_MAX_WAIT_S,
                        "default": 30,
                        "description": "`wait`: seconds to wait for the job to end."
                    }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
            Risk::ReadOnly,
        )
        .with_tags(["shell"])
        .with_timeout(Duration::from_secs(JOBS_MAX_WAIT_S + 60))
    }

    async fn call(
        &self,
        _ctx: &ToolContext,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let input: JobsInput = parse(input)?;
        if input.action == Action::List {
            return Ok(list(&self.jobs.list()));
        }
        let Some(id) = input.job_id.as_deref() else {
            return Err(ToolError::InvalidInput(format!(
                "`job_id` is required for `{}`",
                action_name(input.action)
            )));
        };
        let Some(job) = self.jobs.get(id) else {
            let known = self.jobs.list();
            return Ok(ToolOutput::error(if known.is_empty() {
                format!("no job `{id}`; no jobs have been started")
            } else {
                format!(
                    "no job `{id}`; known jobs: {}",
                    known
                        .iter()
                        .map(|j| j.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }));
        };
        match input.action {
            Action::List | Action::Output => {}
            Action::Wait => {
                let timeout = Duration::from_secs(input.timeout_s.clamp(1, JOBS_MAX_WAIT_S));
                tokio::select! {
                    () = cancel.cancelled() => return Err(ToolError::Cancelled),
                    _ = job.wait(timeout) => {}
                }
            }
            Action::Kill => {
                job.kill();
                job.wait(KILL_WAIT).await;
            }
        }
        Ok(job_output(&job))
    }
}

fn action_name(action: Action) -> &'static str {
    match action {
        Action::List => "list",
        Action::Output => "output",
        Action::Wait => "wait",
        Action::Kill => "kill",
    }
}

/// `<id>\t<status>\t<command>` per job, then `[N jobs]`.
fn list(jobs: &[Arc<Job>]) -> ToolOutput {
    use std::fmt::Write as _;
    let mut text = String::new();
    for job in jobs {
        let _ = writeln!(
            text,
            "{}\t{}\t{}",
            job.id,
            status_line(job),
            super::limits::one_line(&job.command, 80)
        );
    }
    let running = jobs.iter().filter(|j| j.is_running()).count();
    let trailer = match jobs.len() {
        0 => "[no jobs]".to_owned(),
        n => format!("[{n} jobs, {running} running]"),
    };
    text.push_str(&trailer);
    text.push('\n');
    ToolOutput::text(text)
        .with_summary(format!("{} jobs, {running} running", jobs.len()))
        .with_metadata(json!({
            "jobs": jobs.iter().map(|j| json!({
                "job_id": j.id,
                "pid": j.pid,
                "running": j.is_running(),
                "detached": j.detached,
                "agent_id": j.agent_id,
                "description": j.description,
            })).collect::<Vec<_>>(),
        }))
}

/// `running · 12.3 s`, or the finished run's footer.
fn status_line(job: &Job) -> String {
    match &job.state().outcome {
        None => format!("running · {}", fmt_duration(job.started.elapsed())),
        Some(outcome) => Ending::of(outcome, job.timeout).footer,
    }
}

/// The job's output so far (or final), with its status as the footer.
fn job_output(job: &Job) -> ToolOutput {
    let state = job.state();
    let mut out = if let Some(outcome) = &state.outcome {
        render(&state.transcript, outcome, job.timeout)
    } else {
        use std::fmt::Write as _;
        let mut text = state.transcript.text();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        let dur = fmt_duration(job.started.elapsed());
        let _ = writeln!(text, "running · {dur} (pid {})", job.pid);
        ToolOutput::text(text)
            .with_summary(format!("{} running for {dur}", job.id))
            .with_metadata(json!({
                "stdout_bytes": state.transcript.stdout().total(),
                "stderr_bytes": state.transcript.stderr().total(),
                "stdout_lines": state.transcript.stdout().lines(),
                "stderr_lines": state.transcript.stderr().lines(),
                "truncated": state.transcript.truncated(),
            }))
    };
    out.metadata["job_id"] = json!(job.id);
    out.metadata["pid"] = json!(job.pid);
    out.metadata["running"] = json!(state.outcome.is_none());
    out.metadata["detached"] = json!(job.detached);
    out.metadata["description"] = json!(job.description);
    if state.outcome.is_some() {
        out.summary = format!("{}: {}", job.id, out.summary);
    }
    out
}
