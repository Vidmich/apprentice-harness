//! `harness prompt show` (task M01-09): the system blocks the mentor
//! gets, as a session runs under them or as they would be assembled
//! for a workspace, with their token count on request.

use std::path::PathBuf;

use apprentice_api::methods::{PromptShow, PromptShowParams};
use clap::{Args, Subcommand};

use crate::Ctx;
use crate::config::workspace_string;
use crate::daemon::with_client;

#[derive(Debug, Subcommand)]
pub enum PromptCommand {
    /// Print the assembled system prompt: the frozen core and the
    /// workspace block.
    Show(ShowArgs),
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// The prompt of this session (its live blocks when the daemon has
    /// them, else assembled for its workspace).
    #[arg(long, value_name = "ID", conflicts_with = "workspace")]
    session: Option<String>,
    /// Assemble the prompt for this workspace (default: the current
    /// directory).
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
    /// Count the blocks' tokens with the mentor's `count_tokens`
    /// (needs an API key).
    #[arg(long)]
    count: bool,
}

pub fn run(ctx: &Ctx, cmd: &PromptCommand) -> anyhow::Result<()> {
    match cmd {
        PromptCommand::Show(a) => show(ctx, a),
    }
}

fn show(ctx: &Ctx, a: &ShowArgs) -> anyhow::Result<()> {
    let workspace = if a.session.is_some() {
        None
    } else {
        Some(workspace_string(
            &a.workspace.clone().unwrap_or_else(|| PathBuf::from(".")),
        )?)
    };
    let params = PromptShowParams {
        session_id: a.session.clone(),
        workspace,
        count: a.count,
    };
    let r = with_client(
        ctx,
        |c| async move { Ok(c.call::<PromptShow>(params).await?) },
    )?;
    if ctx.out.json {
        return ctx.out.emit_json(&r);
    }
    let source = match (&r.session_id, &r.workspace) {
        (Some(s), _) => format!("session {s}"),
        (None, Some(w)) => format!("workspace {w}"),
        (None, None) => "no workspace".to_owned(),
    };
    ctx.out
        .line(ctx.out.dim(&format!("# {} · {source}", r.version)));
    for (i, block) in r.blocks.iter().enumerate() {
        let cache = if block.cache {
            ", cache breakpoint"
        } else {
            ""
        };
        ctx.out.line(ctx.out.dim(&format!(
            "--- system[{i}] ({} bytes{cache}) ---",
            block.text.len()
        )));
        ctx.out.line(block.text.trim_end_matches('\n'));
    }
    if a.count {
        match (&r.tokens, &r.token_error) {
            (Some(n), _) => ctx
                .out
                .line(ctx.out.dim(&format!("--- {n} input tokens ---"))),
            (None, Some(e)) => ctx.out.info(format!("token count unavailable: {e}")),
            (None, None) => {}
        }
    }
    Ok(())
}
