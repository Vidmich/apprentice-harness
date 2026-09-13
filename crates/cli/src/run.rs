//! `harness run "<prompt>"`: one agent run, streamed. Text goes to stdout
//! as it arrives, tool activity (calls, results, `--show-output` for
//! what a running tool prints, warnings, waits) and the closing usage
//! line to stderr; `--json` prints one event per line followed by a
//! `result` object.
//! CTRL-C sends `agent.cancel` and waits for `agent.finished` (exit 130);
//! a second CTRL-C gives up waiting. A `permission.request` is put to
//! the user on stderr and answered from a line of stdin (task M01-07);
//! with `--json` it is denied at once, this being no place for a prompt.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::Context as _;
use apprentice_api::events::{AgentStatus, Event, LogLevel, Risk};
use apprentice_api::jsonrpc::RpcError;
use apprentice_api::methods::{
    AgentCancel, AgentIdParams, AgentRun, AgentRunParams, PermissionRespond,
    PermissionRespondParams, SessionCreate, SessionCreateParams, SessionList, SessionListParams,
};
use apprentice_api::types::{
    Effort, PermissionAnswer, PermissionDecision, PermissionMode, PermissionSource, RuleSpec,
    RunOptions, Usage,
};
use apprentice_client::{ClientError, DaemonClient};
use clap::{Args, ValueEnum};
use serde::Serialize;

use crate::config::workspace_string;
use crate::daemon::with_client;
use crate::stats::{thousands, usd};
use crate::tools::{describe_rule, risk_name};
use crate::{Ctx, Exit};

#[derive(Debug, Args)]
#[command(after_help = "\
Examples:
  harness run \"summarise the failing tests\"
  harness run --session 0192abc \"now fix the first one\"
  harness run --last \"and the second\"
  harness run --workspace ~/proj --effort high --no-apprentice \"review src/lib.rs\"
  harness run --permission-mode plan \"how is the config loaded?\"
  harness --json run \"say hi\" | jq -c 'select(.type == \"result\")'")]
#[allow(clippy::struct_excessive_bools)] // flags, each its own switch
pub struct RunArgs {
    /// The task for the mentor.
    prompt: String,
    /// Continue an existing session instead of creating one.
    #[arg(long, value_name = "ID", conflicts_with_all = ["workspace", "last"])]
    session: Option<String>,
    /// Continue the most recent session of the workspace.
    #[arg(long)]
    last: bool,
    /// Workspace root for the session (default: current directory).
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
    /// Mentor model for this run (default: `mentor.model` from config).
    #[arg(long, value_name = "MODEL")]
    model: Option<String>,
    /// Mentor effort for this run.
    #[arg(long, value_enum, value_name = "LEVEL")]
    effort: Option<EffortArg>,
    /// Send everything to the mentor; do not involve the apprentice.
    #[arg(long)]
    no_apprentice: bool,
    /// Also print the mentor's thinking, dimmed, on stderr.
    #[arg(long)]
    show_thinking: bool,
    /// Also print what running tools output (the shell's stdout and
    /// stderr), dimmed, on stderr.
    #[arg(long)]
    show_output: bool,
    /// How tool calls are permitted: `default` (rules, then ask), `plan`
    /// (read-only) or `auto` (writes inside the workspace without
    /// asking). Default: `permissions.default_mode` from config.
    #[arg(long, value_enum, value_name = "MODE")]
    permission_mode: Option<PermissionModeArg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PermissionModeArg {
    Default,
    Plan,
    Auto,
}

impl From<PermissionModeArg> for PermissionMode {
    fn from(m: PermissionModeArg) -> Self {
        match m {
            PermissionModeArg::Default => PermissionMode::Default,
            PermissionModeArg::Plan => PermissionMode::Plan,
            PermissionModeArg::Auto => PermissionMode::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EffortArg {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl From<EffortArg> for Effort {
    fn from(e: EffortArg) -> Self {
        match e {
            EffortArg::Low => Effort::Low,
            EffortArg::Medium => Effort::Medium,
            EffortArg::High => Effort::High,
            EffortArg::Xhigh => Effort::XHigh,
            EffortArg::Max => Effort::Max,
        }
    }
}

/// The closing `--json` line.
#[derive(Debug, Serialize)]
struct RunResult {
    r#type: &'static str,
    agent_id: String,
    session_id: String,
    status: AgentStatus,
    usage: Usage,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
    elapsed_s: f64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

pub fn run(ctx: &Ctx, args: &RunArgs) -> anyhow::Result<()> {
    let started = Instant::now();
    let result = with_client(ctx, |client| async move {
        stream(ctx, args, &client, started).await
    })?;
    match result.status {
        AgentStatus::Ok => Ok(()),
        AgentStatus::Cancelled => Err(Exit::Interrupted.into()),
        AgentStatus::Error => Err(match result.error {
            Some(e) => anyhow::Error::from(ClientError::Rpc(e)).context("agent failed"),
            None => anyhow::anyhow!("agent failed"),
        }),
        _ => anyhow::bail!("agent finished with an unknown status"),
    }
}

async fn stream(
    ctx: &Ctx,
    args: &RunArgs,
    client: &DaemonClient,
    started: Instant,
) -> anyhow::Result<RunResult> {
    let out = ctx.out;
    let session_id = if let Some(id) = &args.session {
        id.clone()
    } else if args.last {
        let workspace = args.workspace.clone().unwrap_or_else(|| PathBuf::from("."));
        let r = client
            .call::<SessionList>(SessionListParams {
                workspace: Some(workspace_string(&workspace)?),
                limit: Some(1),
                ..SessionListParams::default()
            })
            .await
            .context("listing sessions")?;
        let Some(last) = r.sessions.into_iter().next() else {
            anyhow::bail!(
                "no session on {} yet; run without --last to start one",
                workspace_string(&workspace)?
            );
        };
        out.info(format!(
            "session {} · {}",
            last.id,
            last.title.as_deref().unwrap_or("(untitled)")
        ));
        last.id
    } else {
        let workspace = args.workspace.clone().unwrap_or_else(|| PathBuf::from("."));
        let r = client
            .call::<SessionCreate>(SessionCreateParams {
                workspace: Some(workspace_string(&workspace)?),
                title: None,
            })
            .await
            .context("creating a session")?;
        out.info(format!("session {}", r.session_id));
        r.session_id
    };
    let params = AgentRunParams {
        session_id: session_id.clone(),
        prompt: args.prompt.clone(),
        options: RunOptions {
            model: args.model.clone(),
            effort: args.effort.map(Effort::from),
            apprentice: args.no_apprentice.then_some(false),
            permission_mode: args.permission_mode.map(PermissionMode::from),
        },
    };
    let (run, mut events) = client
        .call_streaming::<AgentRun>(params)
        .await
        .context("starting the agent")?;
    let agent_id = run.agent_id.clone();

    let mut interrupts = Interrupts::new()?;
    let mut cancel_sent = false;
    let mut stdin = StdinLines::default();
    let mut prompt: Option<Prompt> = None;
    let mut usage = Usage::default();
    let mut cost: Option<f64> = None;
    let mut wrote_text = false;
    let mut at_line_start = true;
    let (status, error, truncated) = loop {
        let ev = tokio::select! {
            ev = events.next() => ev,
            line = stdin.next(), if prompt.is_some() => {
                let Some(p) = prompt.take() else { continue };
                if let Some(answer) = parse_answer(line.as_deref()) {
                    respond(client, out, &p, answer).await;
                } else {
                    out.info("  answer with one of a, s, w, A, d, D");
                    eprint!("{PROMPT_OPTIONS}");
                    crate::out::flush_stderr();
                    prompt = Some(p);
                }
                continue;
            }
            () = interrupts.recv() => {
                if cancel_sent {
                    out.info("giving up waiting for the agent");
                    return Err(Exit::Interrupted.into());
                }
                cancel_sent = true;
                out.info("cancelling (CTRL-C again to stop waiting)");
                let cancel = client.call::<AgentCancel>(AgentIdParams { agent_id: agent_id.clone() });
                if let Err(e) = cancel.await {
                    out.info(format!("agent.cancel failed: {e}"));
                }
                continue;
            }
        };
        let Some(ev) = ev else {
            // The daemon went away mid-run.
            if !at_line_start {
                println!();
            }
            return Err(ClientError::Closed.into());
        };
        if out.json {
            out.emit_json_line(&ev.event)?;
        }
        match ev.event {
            Event::AgentTextDelta { text, .. } => {
                if !out.json {
                    print!("{text}");
                    crate::out::flush();
                }
                if !text.is_empty() {
                    wrote_text = true;
                    at_line_start = text.ends_with('\n');
                }
            }
            Event::AgentThinkingDelta { text, .. } => {
                if args.show_thinking && !out.quiet {
                    eprint!("{}", out.dim(&text));
                }
            }
            Event::AgentToolCall { name, input, .. } => {
                if !at_line_start && !out.json {
                    println!();
                    at_line_start = true;
                }
                out.info(format!("→ {name} {}", compact(&input)));
            }
            Event::AgentToolProgress { text, .. } => {
                if args.show_output && !out.quiet && !out.json {
                    eprint!("{}", out.dim(&text));
                }
            }
            Event::AgentToolResult { ok, summary, .. } => {
                out.info(format!("← {} {summary}", if ok { "ok" } else { "failed" }));
            }
            Event::AgentWarning { kind, message, .. } => {
                out.info(format!("warning ({kind}): {message}"));
            }
            Event::AgentWaiting {
                reason, wait_ms, ..
            } => {
                out.info(format!(
                    "waiting {} s for the mentor ({reason})",
                    wait_ms.div_ceil(1000)
                ));
            }
            Event::AgentUsage {
                usage: u, cost_usd, ..
            } => {
                usage.input_tokens += u.input_tokens;
                usage.output_tokens += u.output_tokens;
                usage.cache_read_input_tokens += u.cache_read_input_tokens;
                usage.cache_creation_input_tokens += u.cache_creation_input_tokens;
                if let Some(c) = cost_usd {
                    cost = Some(cost.unwrap_or(0.0) + c);
                }
            }
            Event::AgentFinished {
                status,
                error,
                truncated,
                ..
            } => break (status, error, truncated),
            Event::PermissionRequest {
                request_id,
                tool,
                risk,
                description,
                suggested_rules,
                timeout_s,
                ..
            } => {
                let p = Prompt {
                    request_id,
                    tool,
                    risk,
                    description,
                    rule: suggested_rules.into_iter().next(),
                    timeout_s,
                };
                if out.json {
                    // A script cannot answer: deny, and say so.
                    out.info(format!("permission for {}: denied (--json)", p.description));
                    respond(client, out, &p, PermissionAnswer::DenyOnce).await;
                } else {
                    if !at_line_start {
                        println!();
                        at_line_start = true;
                    }
                    eprint!("{}", p.text(out));
                    crate::out::flush_stderr();
                    prompt = Some(p);
                }
            }
            Event::PermissionDecision {
                request_id,
                tool,
                decision,
                source,
                reason,
                ..
            } => {
                let answered_here = prompt
                    .as_ref()
                    .is_some_and(|p| request_id.as_deref() == Some(p.request_id.as_str()));
                if answered_here {
                    // Another client (the GUI) answered first.
                    prompt = None;
                    eprintln!();
                    out.info(format!("  {tool}: {} elsewhere", decision_word(decision)));
                } else if decision != PermissionDecision::Allow
                    && !matches!(source, PermissionSource::User)
                {
                    out.info(format!(
                        "✗ {tool}: {}",
                        reason.unwrap_or_else(|| "denied".to_owned())
                    ));
                }
            }
            Event::Log { level, message } => {
                let level = match level {
                    LogLevel::Debug => "debug",
                    LogLevel::Info => "info",
                    LogLevel::Warn => "warn",
                    LogLevel::Error => "error",
                    _ => "log",
                };
                out.info(format!("[{level}] {message}"));
            }
            _ => {}
        }
    };
    if !out.json && wrote_text && !at_line_start {
        println!();
    }
    let elapsed = started.elapsed().as_secs_f64();
    out.info(usage_line(&usage, cost, elapsed));
    if truncated {
        out.info("output truncated: the model hit max_tokens");
    }
    if let Some(e) = &error
        && !out.json
    {
        out.info(format!(
            "agent {}: {}",
            status_word(status),
            crate::out::describe_rpc(e)
        ));
    } else if status != AgentStatus::Ok {
        out.info(format!("agent {}", status_word(status)));
    }
    let result = RunResult {
        r#type: "result",
        agent_id,
        session_id,
        status,
        usage,
        cost_usd: cost,
        elapsed_s: (elapsed * 1000.0).round() / 1000.0,
        truncated,
        error,
    };
    if out.json {
        out.emit_json_line(&result)?;
    }
    Ok(result)
}

fn decision_word(d: PermissionDecision) -> &'static str {
    match d {
        PermissionDecision::Allow => "allowed",
        PermissionDecision::Deny | _ => "denied",
    }
}

/// A `permission.request` waiting for the user's line.
struct Prompt {
    request_id: String,
    tool: String,
    risk: Risk,
    description: String,
    /// What `w` and `A` would write.
    rule: Option<RuleSpec>,
    timeout_s: u64,
}

impl Prompt {
    /// The whole prompt: what would run, the rule an answer would
    /// write, the choices.
    fn text(&self, out: crate::out::Out) -> String {
        let mut s = format!(
            "{} {} ({})\n",
            out.bold("?"),
            self.description,
            risk_name(self.risk)
        );
        if self.description != self.tool && !self.description.starts_with(&self.tool) {
            let _ = writeln!(s, "  tool: {}", self.tool);
        }
        if let Some(rule) = &self.rule {
            let _ = writeln!(s, "  rule for w/A: {}", describe_rule(rule));
        }
        let _ = writeln!(s, "  (denied in {} s without an answer)", self.timeout_s);
        s.push_str(PROMPT_OPTIONS);
        s
    }
}

const PROMPT_OPTIONS: &str =
    "  [a]llow once / [s]ession / [w]orkspace / [A]lways / [d]eny / [D]eny always: ";

/// `a s w A d D`; an empty line or EOF denies once.
fn parse_answer(line: Option<&str>) -> Option<PermissionAnswer> {
    let line = line.map_or("", str::trim);
    Some(match line {
        "" | "d" | "deny" | "n" | "no" => PermissionAnswer::DenyOnce,
        "a" | "allow" | "y" | "yes" | "once" => PermissionAnswer::AllowOnce,
        "s" | "session" => PermissionAnswer::AllowSession,
        "w" | "workspace" => PermissionAnswer::AllowWorkspace,
        "A" | "always" => PermissionAnswer::AllowAlways,
        "D" | "never" => PermissionAnswer::DenyAlways,
        _ => return None,
    })
}

async fn respond(
    client: &DaemonClient,
    out: crate::out::Out,
    p: &Prompt,
    answer: PermissionAnswer,
) {
    let r = client
        .call::<PermissionRespond>(PermissionRespondParams {
            request_id: p.request_id.clone(),
            answer,
            rule: None,
        })
        .await;
    match r {
        Ok(_) => {
            if !out.json {
                let word = match answer {
                    PermissionAnswer::AllowOnce => "allowed once",
                    PermissionAnswer::AllowSession => "allowed for this session",
                    PermissionAnswer::AllowWorkspace => "allowed in this workspace (rule written)",
                    PermissionAnswer::AllowAlways => "always allowed (rule written)",
                    PermissionAnswer::DenyOnce => "denied",
                    PermissionAnswer::DenyAlways | _ => "always denied (rule written)",
                };
                out.info(format!("  {}: {word}", p.tool));
            }
        }
        Err(e) => out.info(format!("  {}: answer not taken: {e}", p.tool)),
    }
}

/// Lines of stdin, read on a thread started at the first prompt; `None`
/// after EOF.
#[derive(Default)]
struct StdinLines {
    rx: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
    eof: bool,
}

impl StdinLines {
    async fn next(&mut self) -> Option<String> {
        if self.eof {
            return None;
        }
        let rx = self.rx.get_or_insert_with(|| {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            std::thread::Builder::new()
                .name("stdin".into())
                .spawn(move || {
                    use std::io::BufRead as _;
                    for line in std::io::stdin().lock().lines() {
                        let Ok(line) = line else { break };
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                })
                .expect("spawning the stdin thread");
            rx
        });
        let line = rx.recv().await;
        if line.is_none() {
            self.eof = true;
        }
        line
    }
}

fn status_word(s: AgentStatus) -> &'static str {
    match s {
        AgentStatus::Ok => "ok",
        AgentStatus::Cancelled => "cancelled",
        AgentStatus::Error => "failed",
        _ => "finished",
    }
}

/// `↳ in 1,204 · out 310 · cache read 0 · $0.0138 · 4.2s`.
pub fn usage_line(u: &Usage, cost: Option<f64>, elapsed_s: f64) -> String {
    let mut line = format!(
        "↳ in {} · out {} · cache read {}",
        thousands(u.input_tokens),
        thousands(u.output_tokens),
        thousands(u.cache_read_input_tokens)
    );
    if u.cache_creation_input_tokens > 0 {
        let _ = write!(
            line,
            " · cache write {}",
            thousands(u.cache_creation_input_tokens)
        );
    }
    if let Some(c) = cost {
        let _ = write!(line, " · {}", usd(c));
    }
    let _ = write!(line, " · {elapsed_s:.1}s");
    line
}

/// One-line JSON, cut to 120 characters for the tool activity log.
fn compact(v: &serde_json::Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 120 {
        let cut: String = s.chars().take(117).collect();
        format!("{cut}...")
    } else {
        s
    }
}

/// CTRL-C (and CTRL-BREAK on Windows) as a stream, so a second press can
/// be told from the first.
struct Interrupts {
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    ctrl_break: tokio::signal::windows::CtrlBreak,
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
}

impl Interrupts {
    fn new() -> std::io::Result<Self> {
        #[cfg(windows)]
        {
            Ok(Self {
                ctrl_c: tokio::signal::windows::ctrl_c()?,
                ctrl_break: tokio::signal::windows::ctrl_break()?,
            })
        }
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Self {
                interrupt: signal(SignalKind::interrupt())?,
                terminate: signal(SignalKind::terminate())?,
            })
        }
    }

    async fn recv(&mut self) {
        #[cfg(windows)]
        {
            tokio::select! {
                _ = self.ctrl_c.recv() => {}
                _ = self.ctrl_break.recv() => {}
            }
        }
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.interrupt.recv() => {}
                _ = self.terminate.recv() => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_line_matches_the_documented_shape() {
        let u = Usage {
            input_tokens: 1204,
            output_tokens: 310,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        assert_eq!(
            usage_line(&u, Some(0.0138), 4.21),
            "↳ in 1,204 · out 310 · cache read 0 · $0.0138 · 4.2s"
        );
        let u = Usage {
            cache_creation_input_tokens: 900,
            ..u
        };
        assert_eq!(
            usage_line(&u, None, 0.04),
            "↳ in 1,204 · out 310 · cache read 0 · cache write 900 · 0.0s"
        );
    }

    #[test]
    fn tool_input_is_cut_short() {
        let long = serde_json::json!({ "text": "x".repeat(300) });
        let s = compact(&long);
        assert_eq!(s.chars().count(), 120);
        assert!(s.ends_with("..."));
        assert_eq!(compact(&serde_json::json!({"a": 1})), "{\"a\":1}");
    }
}
