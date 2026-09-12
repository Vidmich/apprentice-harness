//! `harness session new | list`.

use std::path::PathBuf;

use apprentice_api::methods::{SessionCreate, SessionCreateParams, SessionList, SessionListParams};
use clap::{Args, Subcommand};

use crate::Ctx;
use crate::config::workspace_string;
use crate::daemon::with_client;

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    /// Create a session on a workspace (default: the current directory).
    New(NewArgs),
    /// List sessions, newest first.
    List(ListArgs),
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
    /// Maximum number of rows.
    #[arg(long, value_name = "N", default_value_t = 20)]
    limit: u32,
    /// Skip this many rows first.
    #[arg(long, value_name = "N", default_value_t = 0)]
    offset: u32,
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
        SessionCommand::List(a) => {
            let params = SessionListParams {
                limit: Some(a.limit),
                offset: (a.offset > 0).then_some(a.offset),
            };
            let r = with_client(
                ctx,
                |c| async move { Ok(c.call::<SessionList>(params).await?) },
            )?;
            if ctx.out.json {
                ctx.out.emit_json(&r)?;
            } else {
                let rows: Vec<Vec<String>> = r
                    .sessions
                    .iter()
                    .map(|s| {
                        vec![
                            s.id.clone(),
                            short_time(&s.updated_at),
                            s.title.clone().unwrap_or_default(),
                            s.workspace.clone().unwrap_or_default(),
                        ]
                    })
                    .collect();
                ctx.out
                    .print_table(&["id", "updated", "title", "workspace"], &rows);
            }
        }
    }
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
    #[test]
    fn timestamps_are_trimmed_for_tables() {
        assert_eq!(
            super::short_time("2026-09-12T10:30:00.123456Z"),
            "2026-09-12 10:30:00"
        );
        assert_eq!(super::short_time("odd"), "odd");
    }
}
