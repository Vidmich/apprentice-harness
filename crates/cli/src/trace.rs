//! `harness trace list | show`.

use apprentice_api::methods::{TraceGet, TraceGetParams, TraceList, TraceListParams};
use clap::{Args, Subcommand};

use crate::Ctx;
use crate::daemon::with_client;
use crate::session::short_time;

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
    }
    Ok(())
}
