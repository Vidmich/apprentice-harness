//! `harness trace list | show | export | import | replay-check` (the
//! last three are task M01-14's bundles).

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use apprentice_api::methods::{
    TraceExport, TraceExportParams, TraceExportResult, TraceGet, TraceGetParams, TraceImport,
    TraceImportParams, TraceImportResult, TraceList, TraceListParams, TraceReplayCheck,
    TraceReplayCheckParams,
};
use apprentice_api::types::{ReplayReport, ReplayStatus};
use clap::{Args, Subcommand};

use crate::Ctx;
use crate::daemon::with_client;
use crate::session::short_time;

/// The name of an export without `--output`.
pub const DEFAULT_EXPORT_PREFIX: &str = "harness-traces-";

#[derive(Debug, Subcommand)]
pub enum TraceCommand {
    /// List trace events, newest first.
    ///
    /// Examples:
    ///   harness trace list --session 0192abc --limit 20
    ///   harness trace list --kind agent.finished --kind agent.usage
    #[command(verbatim_doc_comment)]
    List(ListArgs),
    /// Print one event with its payload (and blob with --blob).
    Show(ShowArgs),
    /// Write sessions with their traces and blobs to a bundle.
    ///
    /// A `.tar.zst` path is packed; any other path becomes a directory.
    /// Secrets are redacted unless --no-redact (built-in detectors plus
    /// the patterns of `<config_dir>/redact.toml` and the workspace's
    /// .harness/redact.toml).
    ///
    /// Examples:
    ///   harness trace export --session 0192abc -o fix-tests.tar.zst
    ///   harness trace export --workspace w1 --since 7d --redact-paths
    ///   harness trace export --all --no-redact -o backup/
    #[command(verbatim_doc_comment)]
    Export(ExportArgs),
    /// Read a bundle into this store (every blob is verified first).
    Import(ImportArgs),
    /// Check that every recorded mentor request can be replayed.
    ///
    /// For each call: the body blob exists, hashes to its id and to the
    /// payload's `request_hash`, parses as the wire request and serialises
    /// back to the same bytes; with --rebuild it is also built again from
    /// the stored conversation and compared. Exits 2 on any failure.
    #[command(name = "replay-check", verbatim_doc_comment)]
    ReplayCheck(ReplayCheckArgs),
}

#[derive(Debug, Args)]
pub struct ExportArgs {
    /// A session to export; repeatable.
    #[arg(long = "session", value_name = "ID")]
    sessions: Vec<String>,
    /// Every session of this workspace.
    #[arg(long, value_name = "ID")]
    workspace: Option<String>,
    /// Sessions created at or after this (7d, 2026-09-01, RFC 3339).
    #[arg(long, value_name = "TIME")]
    since: Option<String>,
    /// Sessions created before this (a date includes the whole day).
    #[arg(long, value_name = "TIME")]
    until: Option<String>,
    /// Every session.
    #[arg(long)]
    all: bool,
    /// Where to write; default `harness-traces-<utc time>.tar.zst` here.
    #[arg(short, long, value_name = "PATH")]
    output: Option<PathBuf>,
    /// Skip the redaction pass (the bundle stays on this machine).
    #[arg(long)]
    no_redact: bool,
    /// Also replace the workspace roots and your home directory with
    /// <WS> and <HOME>.
    #[arg(long)]
    redact_paths: bool,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// A bundle directory or `.tar.zst`.
    path: PathBuf,
    /// Attach every imported session to this registered workspace.
    #[arg(long, value_name = "ID")]
    into_workspace: Option<String>,
    /// Keep the bundle's ids (an existing one is an error).
    #[arg(long)]
    keep_ids: bool,
}

#[derive(Debug, Args)]
pub struct ReplayCheckArgs {
    /// Only this session.
    #[arg(long, value_name = "ID", conflicts_with_all = ["agent", "call"])]
    session: Option<String>,
    /// Only this agent.
    #[arg(long, value_name = "ID", conflicts_with = "call")]
    agent: Option<String>,
    /// Only this call.
    #[arg(long, value_name = "ID")]
    call: Option<String>,
    /// Also rebuild each request from the stored conversation.
    #[arg(long)]
    rebuild: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Only this session.
    #[arg(long, value_name = "ID")]
    session: Option<String>,
    /// Only this agent.
    #[arg(long, value_name = "ID")]
    agent: Option<String>,
    /// Only these event kinds; repeatable.
    #[arg(long = "kind", value_name = "KIND")]
    kinds: Vec<String>,
    /// Maximum number of rows.
    #[arg(long, value_name = "N", default_value_t = 50)]
    limit: u32,
    /// Only events with a sequence number below this (paging backwards).
    #[arg(long, value_name = "SEQ")]
    before: Option<u64>,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Event id (from `trace list`).
    event_id: String,
    /// Also print the attached blob (tool output, raw response) as text.
    #[arg(long)]
    blob: bool,
}

pub fn run(ctx: &Ctx, cmd: &TraceCommand) -> anyhow::Result<()> {
    match cmd {
        TraceCommand::List(a) => {
            let params = TraceListParams {
                session_id: a.session.clone(),
                agent_id: a.agent.clone(),
                kinds: a.kinds.clone(),
                limit: Some(a.limit),
                before_seq: a.before,
            };
            let r = with_client(
                ctx,
                |c| async move { Ok(c.call::<TraceList>(params).await?) },
            )?;
            if ctx.out.json {
                ctx.out.emit_json(&r)?;
            } else {
                let rows: Vec<Vec<String>> = r
                    .events
                    .iter()
                    .map(|e| {
                        vec![
                            e.id.clone(),
                            e.seq.to_string(),
                            short_time(&e.ts),
                            e.kind.clone(),
                            e.session_id.clone(),
                            e.agent_id.clone().unwrap_or_default(),
                            e.blob_bytes.map(|b| b.to_string()).unwrap_or_default(),
                        ]
                    })
                    .collect();
                ctx.out.print_table(
                    &["id", "seq", "time", "kind", "session", "agent", "blob"],
                    &rows,
                );
            }
        }
        TraceCommand::Show(a) => {
            let params = TraceGetParams {
                event_id: a.event_id.clone(),
                include_blob: a.blob,
            };
            let r = with_client(
                ctx,
                |c| async move { Ok(c.call::<TraceGet>(params).await?) },
            )?;
            if ctx.out.json {
                ctx.out.emit_json(&r)?;
            } else {
                let s = &r.event.summary;
                let mut pairs = vec![
                    ("id", s.id.clone()),
                    ("kind", s.kind.clone()),
                    ("time", s.ts.clone()),
                    ("seq", s.seq.to_string()),
                    ("session", s.session_id.clone()),
                ];
                if let Some(agent) = &s.agent_id {
                    pairs.push(("agent", agent.clone()));
                }
                if let Some(step) = &s.step_id {
                    pairs.push(("step", step.clone()));
                }
                if let Some(blob) = &r.event.blob_id {
                    pairs.push((
                        "blob",
                        format!("{blob} ({} bytes)", s.blob_bytes.unwrap_or(0)),
                    ));
                }
                ctx.out.print_kv(&pairs);
                ctx.out.line("");
                ctx.out
                    .line(serde_json::to_string_pretty(&r.event.payload)?);
                if let Some(blob) = &r.blob {
                    ctx.out.line("");
                    ctx.out.line(ctx.out.dim("--- blob ---"));
                    ctx.out.line(blob);
                }
            }
        }
        TraceCommand::Export(a) => {
            let output = match &a.output {
                Some(p) => p.clone(),
                None => PathBuf::from(default_export_name(&time::OffsetDateTime::now_utc())),
            };
            let params = TraceExportParams {
                output: absolute(&output)?,
                session_ids: a.sessions.clone(),
                workspace_id: a.workspace.clone(),
                since: a.since.clone(),
                until: a.until.clone(),
                all: a.all,
                redact: !a.no_redact,
                redact_paths: a.redact_paths,
            };
            let r = with_client(
                ctx,
                |c| async move { Ok(c.call::<TraceExport>(params).await?) },
            )?;
            if ctx.out.json {
                ctx.out.emit_json(&r)?;
            } else {
                ctx.out.line(format_export(&r));
            }
        }
        TraceCommand::Import(a) => {
            let params = TraceImportParams {
                path: absolute(&a.path)?,
                into_workspace: a.into_workspace.clone(),
                keep_ids: a.keep_ids,
            };
            let r = with_client(
                ctx,
                |c| async move { Ok(c.call::<TraceImport>(params).await?) },
            )?;
            if ctx.out.json {
                ctx.out.emit_json(&r)?;
            } else {
                ctx.out.line(format_import(&r));
            }
        }
        TraceCommand::ReplayCheck(a) => {
            let params = TraceReplayCheckParams {
                session_id: a.session.clone(),
                agent_id: a.agent.clone(),
                call_id: a.call.clone(),
                rebuild: a.rebuild,
            };
            let r = with_client(ctx, |c| async move {
                Ok(c.call::<TraceReplayCheck>(params).await?)
            })?;
            if ctx.out.json {
                ctx.out.emit_json(&r)?;
            } else {
                ctx.out.line(format_replay(&r));
            }
            if r.failed > 0 {
                anyhow::bail!("{} of {} calls failed replay-check", r.failed, r.checked);
            }
        }
    }
    Ok(())
}

/// The path as the daemon needs it: absolute, from our working directory.
fn absolute(path: &Path) -> anyhow::Result<String> {
    let p = std::path::absolute(path).with_context(|| format!("resolving {}", path.display()))?;
    Ok(p.to_string_lossy().into_owned())
}

/// `harness-traces-20260913-1012Z.tar.zst` for a UTC time.
pub fn default_export_name(now: &time::OffsetDateTime) -> String {
    const FORMAT: &[time::format_description::BorrowedFormatItem<'_>] =
        time::macros::format_description!("[year][month][day]-[hour][minute]Z");
    format!(
        "{DEFAULT_EXPORT_PREFIX}{}.tar.zst",
        now.to_offset(time::UtcOffset::UTC)
            .format(FORMAT)
            .expect("fixed format")
    )
}

/// Human output of `trace export`: where, what, and the redaction.
pub fn format_export(r: &TraceExportResult) -> String {
    let m = &r.manifest;
    let c = &m.counts;
    let mut lines = vec![
        format!("wrote {}", r.path),
        format!(
            "sessions {} · workspaces {} · agents {} · steps {} · events {} · mentor calls {} · messages {} · blobs {} ({})",
            c.sessions,
            c.workspaces,
            c.agents,
            c.steps,
            c.events,
            c.mentor_calls,
            c.messages,
            c.blobs,
            human_bytes(c.blob_bytes)
        ),
    ];
    match &m.redaction {
        None => lines.push("redaction: off".to_owned()),
        Some(red) if !red.applied => lines.push("redaction: nothing matched".to_owned()),
        Some(red) => {
            let touched = if red.touched_requests > 0 {
                format!(
                    "; {} request bodies touched — not byte-replayable",
                    red.touched_requests
                )
            } else {
                String::new()
            };
            lines.push(format!(
                "redaction: {} replacements ({} distinct secrets){touched}",
                red.replacements, red.secrets
            ));
            let matched: Vec<_> = red.rules.iter().filter(|r| r.matches > 0).collect();
            let width = matched
                .iter()
                .map(|r| r.name.chars().count())
                .max()
                .unwrap_or(0);
            for rule in matched {
                lines.push(format!(
                    "  {:<width$}  {:<9}  {}",
                    rule.name, rule.source, rule.matches
                ));
            }
        }
    }
    lines.join("\n")
}

/// Human output of `trace import`.
pub fn format_import(r: &TraceImportResult) -> String {
    let c = &r.counts;
    let renamed: Vec<String> = r
        .sessions
        .iter()
        .filter(|s| s.from != s.to)
        .map(|s| format!("{} → {}", s.from, s.to))
        .collect();
    let new_ids = if renamed.is_empty() {
        String::new()
    } else {
        format!(" (new ids: {})", renamed.join(", "))
    };
    let mut lines = vec![
        format!(
            "imported {} session{}{new_ids}",
            r.sessions.len(),
            if r.sessions.len() == 1 { "" } else { "s" }
        ),
        format!(
            "agents {} · steps {} · events {} · mentor calls {} · messages {} · blobs {} ({} new)",
            c.agents, c.steps, c.events, c.mentor_calls, c.messages, c.blobs, r.blobs_written
        ),
    ];
    if r.redacted {
        lines.push(
            "the bundle was redacted: its request bodies are not the ones the mentor saw"
                .to_owned(),
        );
    }
    lines.join("\n")
}

/// Human output of `replay-check`: the calls that did not pass, then
/// the count line.
pub fn format_replay(r: &ReplayReport) -> String {
    let mut lines = Vec::new();
    for c in r.calls.iter().filter(|c| c.status != ReplayStatus::Ok) {
        let status = match c.status {
            ReplayStatus::Ok => "ok",
            ReplayStatus::Failed => "FAILED",
            ReplayStatus::Skipped => "skipped",
        };
        lines.push(format!(
            "{status:<7} {} ({} · session {} · {})",
            c.call_id,
            c.kind,
            c.session_id,
            c.checks.last().map_or("-", String::as_str)
        ));
        for p in &c.problems {
            lines.push(format!("        {p}"));
        }
    }
    lines.push(format!(
        "checked {} · passed {} · failed {} · skipped {}{}",
        r.checked,
        r.passed,
        r.failed,
        r.skipped,
        if r.rebuild { " · rebuilt" } else { "" }
    ));
    lines.join("\n")
}

#[allow(clippy::cast_precision_loss)] // display only
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

#[cfg(test)]
mod tests {
    use apprentice_api::types::{
        BundleCounts, BundleManifest, BundleSelection, ImportedSession, RedactionReport,
        RedactionRule, ReplayCall,
    };

    use super::*;

    fn counts() -> BundleCounts {
        BundleCounts {
            sessions: 2,
            workspaces: 1,
            agents: 3,
            steps: 9,
            events: 120,
            mentor_calls: 9,
            messages: 24,
            blobs: 40,
            blob_bytes: 1_572_864,
        }
    }

    fn manifest(redaction: Option<RedactionReport>) -> BundleManifest {
        BundleManifest {
            format_version: 1,
            created_at: "t".into(),
            harness_version: "0.1.0".into(),
            schema_version: 3,
            selection: BundleSelection::default(),
            sessions: vec![],
            counts: counts(),
            redaction,
        }
    }

    #[test]
    fn export_output_lists_counts_and_the_rules_that_matched() {
        let r = TraceExportResult {
            path: "C:/x/week.tar.zst".into(),
            manifest: manifest(Some(RedactionReport {
                applied: true,
                rules: vec![
                    RedactionRule {
                        name: "anthropic_key".into(),
                        source: "builtin".into(),
                        matches: 4,
                    },
                    RedactionRule {
                        name: "pem".into(),
                        source: "builtin".into(),
                        matches: 0,
                    },
                    RedactionRule {
                        name: "paths".into(),
                        source: "paths".into(),
                        matches: 12,
                    },
                ],
                replacements: 16,
                secrets: 1,
                paths: true,
                blob_map: std::collections::BTreeMap::default(),
                touched_requests: 6,
                replayable: false,
            })),
        };
        assert_eq!(
            format_export(&r),
            [
                "wrote C:/x/week.tar.zst",
                "sessions 2 · workspaces 1 · agents 3 · steps 9 · events 120 · mentor calls 9 · messages 24 · blobs 40 (1.5 MiB)",
                "redaction: 16 replacements (1 distinct secrets); 6 request bodies touched — not byte-replayable",
                "  anthropic_key  builtin    4",
                "  paths          paths      12",
            ]
            .join("\n")
        );
        let off = TraceExportResult {
            path: "C:/x/b".into(),
            manifest: manifest(None),
        };
        assert!(format_export(&off).ends_with("redaction: off"));
        let clean = TraceExportResult {
            path: "C:/x/b".into(),
            manifest: manifest(Some(RedactionReport::default())),
        };
        assert!(format_export(&clean).ends_with("redaction: nothing matched"));
    }

    #[test]
    fn import_and_replay_outputs() {
        let r = TraceImportResult {
            sessions: vec![
                ImportedSession {
                    from: "s1".into(),
                    to: "s1".into(),
                },
                ImportedSession {
                    from: "s2".into(),
                    to: "s9".into(),
                },
            ],
            counts: counts(),
            redacted: true,
            blobs_written: 38,
        };
        assert_eq!(
            format_import(&r),
            [
                "imported 2 sessions (new ids: s2 → s9)",
                "agents 3 · steps 9 · events 120 · mentor calls 9 · messages 24 · blobs 40 (38 new)",
                "the bundle was redacted: its request bodies are not the ones the mentor saw",
            ]
            .join("\n")
        );
        let report = ReplayReport {
            calls: vec![
                ReplayCall {
                    call_id: "m1".into(),
                    session_id: "s1".into(),
                    agent_id: "a1".into(),
                    step_id: "st1".into(),
                    kind: "step".into(),
                    request_event_id: "e1".into(),
                    status: ReplayStatus::Ok,
                    checks: vec!["blob".into()],
                    problems: vec![],
                },
                ReplayCall {
                    call_id: "m2".into(),
                    session_id: "s1".into(),
                    agent_id: "a1".into(),
                    step_id: "st2".into(),
                    kind: "step".into(),
                    request_event_id: "e2".into(),
                    status: ReplayStatus::Failed,
                    checks: vec!["blob".into(), "hash".into()],
                    problems: vec!["body hashes to b, not its id a".into()],
                },
            ],
            checked: 2,
            passed: 1,
            failed: 1,
            skipped: 0,
            rebuild: true,
        };
        assert_eq!(
            format_replay(&report),
            [
                "FAILED  m2 (step · session s1 · hash)",
                "        body hashes to b, not its id a",
                "checked 2 · passed 1 · failed 1 · skipped 0 · rebuilt",
            ]
            .join("\n")
        );
        assert_eq!(
            default_export_name(&time::macros::datetime!(2026-09-13 10:12:00 UTC)),
            "harness-traces-20260913-1012Z.tar.zst"
        );
        assert_eq!(human_bytes(512), "512 B");
    }
}
