//! `harness session new | list | show | search | rename | archive |
//! delete | export | mark` (task M01-10 added everything after `list`;
//! `mark` is task M01-15).

use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::Context as _;
use apprentice_api::methods::{
    SessionArchive, SessionArchiveParams, SessionCreate, SessionCreateParams, SessionDelete,
    SessionDeleteParams, SessionExportMethod, SessionGet, SessionGetParams, SessionIdParams,
    SessionList, SessionListParams, SessionMarkMethod, SessionMarkParams, SessionRename,
    SessionRenameParams, SessionSearch, SessionSearchParams,
};
use apprentice_api::types::{AgentSummary, SessionMark, SessionMessage, SessionSummary};
use clap::{Args, Subcommand, ValueEnum};
use serde_json::Value;

use crate::Ctx;
use crate::config::workspace_string;
use crate::daemon::with_client;
use crate::stats::{thousands, usd};

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// Create a session on a workspace (default: the current directory).
    New(NewArgs),
    /// List sessions, most recent activity first.
    List(ListArgs),
    /// Print a session's conversation.
    Show(ShowArgs),
    /// Full-text search over every session's messages.
    Search(SearchArgs),
    /// Give a session a title (the generator never replaces it).
    Rename(RenameArgs),
    /// Hide a session from the list (or bring it back with --undo).
    Archive(ArchiveArgs),
    /// Delete a session's conversation (traces stay unless --purge-traces).
    Delete(DeleteArgs),
    /// Write a session as one JSON document.
    Export(ExportArgs),
    /// Record your verdict on the last run: accept, reject, or task done.
    Mark(MarkArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mark {
    /// The run's result is good (`outcome.user_accept`).
    Accept,
    /// The run's result is not (`outcome.user_reject`).
    Reject,
    /// The session's task is complete (`outcome.task_done`).
    Done,
}

impl From<Mark> for SessionMark {
    fn from(m: Mark) -> Self {
        match m {
            Mark::Accept => Self::Accept,
            Mark::Reject => Self::Reject,
            Mark::Done => Self::Done,
        }
    }
}

#[derive(Debug, Args)]
pub struct MarkArgs {
    id: String,
    /// The verdict.
    #[arg(value_enum)]
    mark: Mark,
    /// A word on why.
    #[arg(long, value_name = "TEXT")]
    note: Option<String>,
    /// The run to mark (default: the session's last).
    #[arg(long, value_name = "ID")]
    agent: Option<String>,
}

#[derive(Debug, Args)]
pub struct NewArgs {
    /// Workspace root the session works in.
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
    /// A title for listings.
    #[arg(long, value_name = "TEXT")]
    title: Option<String>,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Only sessions whose title or messages contain these words.
    #[arg(long, value_name = "WORDS")]
    query: Option<String>,
    /// Only sessions created on this workspace root.
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
    /// Also archived and deleted sessions.
    #[arg(long)]
    all: bool,
    /// Maximum number of rows.
    #[arg(long, value_name = "N", default_value_t = 20)]
    limit: u32,
    /// Skip this many rows first.
    #[arg(long, value_name = "N", default_value_t = 0)]
    offset: u32,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// The session.
    id: String,
    /// Messages with a position above this.
    #[arg(long, value_name = "SEQ", conflicts_with = "tail")]
    after: Option<u64>,
    /// The newest messages instead of the oldest (with --limit).
    #[arg(long)]
    tail: bool,
    /// Maximum number of messages.
    #[arg(long, value_name = "N")]
    limit: Option<u32>,
    /// Print tool inputs and results in full.
    #[arg(long)]
    full: bool,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Words to find (each as a prefix, all required).
    query: String,
    /// Also archived and deleted sessions.
    #[arg(long)]
    all: bool,
    /// Maximum number of hits.
    #[arg(long, value_name = "N", default_value_t = 20)]
    limit: u32,
}

#[derive(Debug, Args)]
pub struct RenameArgs {
    id: String,
    /// The new title.
    title: String,
}

#[derive(Debug, Args)]
pub struct ArchiveArgs {
    id: String,
    /// Reopen an archived session.
    #[arg(long)]
    undo: bool,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    id: String,
    /// Also remove every event, call and agent of the session.
    #[arg(long)]
    purge_traces: bool,
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    id: String,
    /// Write here instead of stdout.
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,
}

pub fn run(ctx: &Ctx, cmd: &SessionCommand) -> anyhow::Result<()> {
    match cmd {
        SessionCommand::New(a) => {
            let params = SessionCreateParams {
                workspace: Some(workspace_string(
                    &a.workspace.clone().unwrap_or_else(|| PathBuf::from(".")),
                )?),
                title: a.title.clone(),
            };
            let r = with_client(ctx, |c| async move {
                Ok(c.call::<SessionCreate>(params).await?)
            })?;
            if ctx.out.json {
                ctx.out.emit_json(&r)?;
            } else {
                ctx.out.line(&r.session_id);
            }
        }
        SessionCommand::List(a) => list(ctx, a)?,
        SessionCommand::Show(a) => show(ctx, a)?,
        SessionCommand::Search(a) => search(ctx, a)?,
        SessionCommand::Rename(a) => {
            let params = SessionRenameParams {
                id: a.id.clone(),
                title: a.title.clone(),
            };
            with_client(ctx, |c| async move {
                Ok(c.call::<SessionRename>(params).await?)
            })?;
            if ctx.out.json {
                ctx.out
                    .emit_json(&serde_json::json!({ "id": a.id, "title": a.title }))?;
            } else {
                ctx.out.info(format!("renamed {}", a.id));
            }
        }
        SessionCommand::Archive(a) => {
            let params = SessionArchiveParams {
                id: a.id.clone(),
                archived: !a.undo,
            };
            with_client(ctx, |c| async move {
                Ok(c.call::<SessionArchive>(params).await?)
            })?;
            let verb = if a.undo { "reopened" } else { "archived" };
            if ctx.out.json {
                ctx.out
                    .emit_json(&serde_json::json!({ "id": a.id, "archived": !a.undo }))?;
            } else {
                ctx.out.info(format!("{verb} {}", a.id));
            }
        }
        SessionCommand::Delete(a) => {
            let params = SessionDeleteParams {
                id: a.id.clone(),
                purge_traces: a.purge_traces,
            };
            let r = with_client(ctx, |c| async move {
                Ok(c.call::<SessionDelete>(params).await?)
            })?;
            if ctx.out.json {
                ctx.out.emit_json(&r)?;
            } else if a.purge_traces {
                ctx.out.info(format!(
                    "deleted {}: {} messages, {} events",
                    a.id, r.messages_deleted, r.events_deleted
                ));
            } else {
                ctx.out.info(format!(
                    "deleted the conversation of {} ({} messages); traces kept",
                    a.id, r.messages_deleted
                ));
            }
        }
        SessionCommand::Mark(a) => {
            let params = SessionMarkParams {
                id: a.id.clone(),
                mark: a.mark.into(),
                note: a.note.clone(),
                agent_id: a.agent.clone(),
            };
            let r = with_client(ctx, |c| async move {
                Ok(c.call::<SessionMarkMethod>(params).await?)
            })?;
            if ctx.out.json {
                ctx.out.emit_json(&r)?;
            } else {
                ctx.out.info(format!(
                    "{} · run {} of {}: {}",
                    r.outcome.kind, r.agent_id, a.id, r.outcome.summary
                ));
            }
        }
        SessionCommand::Export(a) => {
            let params = SessionIdParams { id: a.id.clone() };
            let r = with_client(ctx, |c| async move {
                Ok(c.call::<SessionExportMethod>(params).await?)
            })?;
            match &a.output {
                Some(path) => {
                    let text = serde_json::to_string_pretty(&r)?;
                    std::fs::write(path, text)
                        .with_context(|| format!("writing {}", path.display()))?;
                    if ctx.out.json {
                        ctx.out.emit_json(&serde_json::json!({
                            "id": a.id,
                            "path": path.display().to_string(),
                            "messages": r.messages.len(),
                            "mentor_calls": r.mentor_calls.len(),
                        }))?;
                    } else {
                        ctx.out.info(format!(
                            "wrote {} ({} messages, {} mentor calls)",
                            path.display(),
                            r.messages.len(),
                            r.mentor_calls.len()
                        ));
                    }
                }
                None => ctx.out.emit_json(&r)?,
            }
        }
    }
    Ok(())
}

fn list(ctx: &Ctx, a: &ListArgs) -> anyhow::Result<()> {
    let params = SessionListParams {
        query: a.query.clone(),
        workspace: a.workspace.as_ref().map(workspace_string).transpose()?,
        workspace_id: None,
        include_archived: a.all,
        limit: Some(a.limit),
        offset: (a.offset > 0).then_some(a.offset),
    };
    let r = with_client(
        ctx,
        |c| async move { Ok(c.call::<SessionList>(params).await?) },
    )?;
    if ctx.out.json {
        return ctx.out.emit_json(&r);
    }
    let rows: Vec<Vec<String>> = r.sessions.iter().map(|s| summary_row(s, a.all)).collect();
    let mut header = vec![
        "id",
        "activity",
        "msgs",
        "tokens",
        "cost",
        "title",
        "workspace",
    ];
    if a.all {
        header.push("status");
    }
    ctx.out.print_table(&header, &rows);
    Ok(())
}

fn summary_row(s: &SessionSummary, with_status: bool) -> Vec<String> {
    let tokens = s.usage.input_tokens
        + s.usage.output_tokens
        + s.usage.cache_read_input_tokens
        + s.usage.cache_creation_input_tokens;
    let mut row = vec![
        s.id.clone(),
        short_time(&s.last_activity),
        s.message_count.to_string(),
        thousands(tokens),
        s.cost_usd.map(usd).unwrap_or_default(),
        s.title.clone().unwrap_or_default(),
        s.workspace.clone().unwrap_or_default(),
    ];
    if with_status {
        row.push(s.status.clone());
    }
    row
}

fn show(ctx: &Ctx, a: &ShowArgs) -> anyhow::Result<()> {
    let params = SessionGetParams {
        id: a.id.clone(),
        after_seq: a.after,
        before_seq: a.tail.then_some(u64::MAX),
        limit: a.limit,
    };
    let r = with_client(
        ctx,
        |c| async move { Ok(c.call::<SessionGet>(params).await?) },
    )?;
    if ctx.out.json {
        return ctx.out.emit_json(&r);
    }
    let s = &r.session.summary;
    ctx.out.line(ctx.out.bold(&format!(
        "{} · {}",
        s.id,
        s.title.as_deref().unwrap_or("(untitled)")
    )));
    let mut facts = vec![
        ("workspace", s.workspace.clone().unwrap_or_default()),
        ("status", s.status.clone()),
        ("created", short_time(&s.created_at)),
        ("activity", short_time(&s.last_activity)),
        ("messages", s.message_count.to_string()),
    ];
    if let Some(v) = &r.session.prompt_version {
        facts.push(("prompt", v.clone()));
    }
    ctx.out.print_kv(&facts);
    if let Some(line) = outcomes_line(&r.agents) {
        ctx.out.line(ctx.out.dim(&line));
    }
    for m in &r.messages {
        ctx.out.line("");
        ctx.out.line(ctx.out.dim(&format!(
            "--- {} #{} ({}) ---",
            m.role,
            m.seq,
            short_time(&m.created_at)
        )));
        ctx.out
            .line(render_message(m, a.full).trim_end_matches('\n'));
    }
    if r.has_more {
        ctx.out.line("");
        ctx.out.info(format!(
            "more messages follow: --after {}",
            r.messages.last().map_or(0, |m| m.seq)
        ));
    }
    Ok(())
}

/// One line per run with outcomes: `run a1: 12 passed (cargo test) ·
/// 3 files (2 changed, 1 added) · accepted`.
pub fn outcomes_line(agents: &[AgentSummary]) -> Option<String> {
    let lines: Vec<String> = agents
        .iter()
        .filter(|a| !a.outcomes.is_empty())
        .map(|a| {
            format!(
                "run {}: {}",
                a.id,
                a.outcomes
                    .iter()
                    .map(|o| o.summary.as_str())
                    .collect::<Vec<_>>()
                    .join(" · ")
            )
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// The blocks of a message as text: text as is, tool calls as
/// `→ name {input}`, results as `← id: text` (cut unless `full`),
/// thinking as a dimmed marker.
pub fn render_message(m: &SessionMessage, full: bool) -> String {
    let mut out = String::new();
    let Some(blocks) = m.content.as_array() else {
        return m.content.to_string();
    };
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                out.push_str(b["text"].as_str().unwrap_or_default());
                out.push('\n');
            }
            Some("tool_use") => {
                let input = b["input"].to_string();
                let _ = writeln!(
                    out,
                    "→ {} {}",
                    b["name"].as_str().unwrap_or("?"),
                    if full { input } else { cut(&input, 200) }
                );
            }
            Some("tool_result") => {
                let text = result_text(&b["content"]);
                let marker = if b["is_error"].as_bool().unwrap_or(false) {
                    "← error"
                } else {
                    "←"
                };
                let _ = writeln!(
                    out,
                    "{marker} {}: {}",
                    b["tool_use_id"].as_str().unwrap_or("?"),
                    if full { text } else { cut(&text, 200) }
                );
            }
            Some("thinking") => out.push_str("[thinking]\n"),
            Some("redacted_thinking") => out.push_str("[redacted thinking]\n"),
            Some(other) => {
                let _ = writeln!(out, "[{other}]");
            }
            None => {
                let _ = writeln!(out, "{b}");
            }
        }
    }
    out
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

/// The first `max` characters on one line, with an ellipsis.
fn cut(text: &str, max: usize) -> String {
    let one_line = text.replace(['\n', '\r'], " ");
    let mut chars = one_line.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn search(ctx: &Ctx, a: &SearchArgs) -> anyhow::Result<()> {
    let params = SessionSearchParams {
        query: a.query.clone(),
        include_archived: a.all,
        limit: Some(a.limit),
    };
    let r = with_client(ctx, |c| async move {
        Ok(c.call::<SessionSearch>(params).await?)
    })?;
    if ctx.out.json {
        return ctx.out.emit_json(&r);
    }
    if r.hits.is_empty() {
        ctx.out.info("no match");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = r
        .hits
        .iter()
        .map(|h| {
            vec![
                h.session_id.clone(),
                format!("#{}", h.seq),
                h.role.clone(),
                h.snippet.replace(['\n', '\r'], " "),
                h.title.clone().unwrap_or_default(),
            ]
        })
        .collect();
    ctx.out
        .print_table(&["session", "msg", "role", "match", "title"], &rows);
    Ok(())
}

/// `2026-09-12T10:30:00.123456Z` → `2026-09-12 10:30:00`.
pub fn short_time(ts: &str) -> String {
    let mut s: String = ts.chars().take(19).collect();
    if let Some(i) = s.find('T') {
        s.replace_range(i..=i, " ");
    }
    s
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn timestamps_are_trimmed_for_tables() {
        assert_eq!(
            super::short_time("2026-09-12T10:30:00.123456Z"),
            "2026-09-12 10:30:00"
        );
        assert_eq!(super::short_time("odd"), "odd");
    }

    #[test]
    fn messages_render_their_blocks() {
        let m = SessionMessage {
            seq: 2,
            role: "assistant".into(),
            content: json!([
                {"type": "thinking", "thinking": "hm", "signature": "s"},
                {"type": "text", "text": "Reading."},
                {"type": "tool_use", "id": "t1", "name": "read_file", "input": {"path": "a.rs"}},
            ]),
            agent_id: None,
            step_id: None,
            created_at: "2026-09-12T10:00:00.000Z".into(),
        };
        assert_eq!(
            render_message(&m, false),
            "[thinking]\nReading.\n→ read_file {\"path\":\"a.rs\"}\n"
        );
        let r = SessionMessage {
            seq: 3,
            role: "user".into(),
            content: json!([
                {"type": "tool_result", "tool_use_id": "t1", "content": [{"type": "text", "text": "fn main() {}\n"}]},
                {"type": "tool_result", "tool_use_id": "t2", "content": "nope", "is_error": true},
            ]),
            agent_id: None,
            step_id: None,
            created_at: String::new(),
        };
        assert_eq!(
            render_message(&r, false),
            "← t1: fn main() {} \n← error t2: nope\n"
        );
        assert_eq!(cut(&"x".repeat(5), 3), "xxx…");
    }

    #[test]
    fn runs_with_outcomes_get_one_line_each() {
        use apprentice_api::types::{AgentSummary, OutcomeInfo, Usage};
        let agent = |id: &str, outcomes: Vec<&str>| AgentSummary {
            id: id.into(),
            status: "ok".into(),
            started_at: String::new(),
            ended_at: None,
            model: None,
            calls: 1,
            usage: Usage::default(),
            cost_usd: None,
            error: None,
            outcomes: outcomes
                .into_iter()
                .map(|s| OutcomeInfo {
                    event_id: "e".into(),
                    kind: "tests".into(),
                    summary: s.into(),
                    ok: Some(true),
                    details: json!({}),
                    at: String::new(),
                })
                .collect(),
        };
        assert_eq!(outcomes_line(&[agent("a1", vec![])]), None);
        assert_eq!(
            outcomes_line(&[
                agent("a1", vec!["3 passed (cargo test)", "1 file added"]),
                agent("a2", vec![]),
                agent("a3", vec!["accepted"]),
            ])
            .unwrap(),
            "run a1: 3 passed (cargo test) · 1 file added
run a3: accepted"
        );
    }
}
