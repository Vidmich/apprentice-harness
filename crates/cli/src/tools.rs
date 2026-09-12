//! `harness tools list` (task M01-01). `allow`, `deny` and `rules` arrive
//! with the permission engine (M01-07).

use std::path::PathBuf;

use apprentice_api::methods::{ToolsList, ToolsListParams};
use clap::{Args, Subcommand};

use crate::Ctx;
use crate::config::workspace_string;
use crate::daemon::with_client;

#[derive(Debug, Subcommand)]
pub enum ToolsCommand {
    /// List the tools the daemon offers the mentor and whether each is
    /// enabled for a workspace.
    List(ListArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Resolve `tools.disabled` for this workspace (default: the user
    /// config alone).
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
    /// Show each tool's description too.
    #[arg(long)]
    describe: bool,
}

pub fn run(ctx: &Ctx, cmd: &ToolsCommand) -> anyhow::Result<()> {
    match cmd {
        ToolsCommand::List(a) => {
            let params = ToolsListParams {
                workspace: a.workspace.as_ref().map(workspace_string).transpose()?,
            };
            let r = with_client(
                ctx,
                |c| async move { Ok(c.call::<ToolsList>(params).await?) },
            )?;
            if ctx.out.json {
                ctx.out.emit_json(&r)?;
            } else if r.tools.is_empty() {
                ctx.out.info("no tools registered");
            } else {
                let rows: Vec<Vec<String>> = r
                    .tools
                    .iter()
                    .map(|t| {
                        let mut row = vec![
                            t.name.clone(),
                            risk_name(t.risk).to_owned(),
                            if t.enabled { "yes" } else { "no" }.to_owned(),
                            t.tags.join(","),
                        ];
                        if a.describe {
                            row.push(t.description.lines().next().unwrap_or("").to_owned());
                        }
                        row
                    })
                    .collect();
                let mut header = vec!["name", "risk", "enabled", "tags"];
                if a.describe {
                    header.push("description");
                }
                ctx.out.print_table(&header, &rows);
            }
        }
    }
    Ok(())
}

fn risk_name(risk: apprentice_api::events::Risk) -> &'static str {
    use apprentice_api::events::Risk;
    match risk {
        Risk::ReadOnly => "read-only",
        Risk::Write => "write",
        Risk::Execute => "execute",
        Risk::Network => "network",
        _ => "other",
    }
}
