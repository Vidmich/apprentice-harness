//! `harness auth set-key | status`. The key travels to the daemon once and
//! is never printed, logged or echoed.

use std::io::{IsTerminal, Read as _};

use anyhow::Context as _;
use apprentice_api::methods::{AuthSetKey, AuthSetKeyParams, AuthStatus, Empty};
use clap::{Args, Subcommand};

use crate::Ctx;
use crate::daemon::with_client;

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Store a provider API key in the daemon's secret store.
    ///
    /// Prompts with hidden input on a terminal; reads the key from stdin
    /// with --stdin or when stdin is not a terminal.
    ///
    /// Examples:
    ///   harness auth set-key
    ///   echo $ANTHROPIC_API_KEY | harness auth set-key --stdin
    #[command(verbatim_doc_comment)]
    #[allow(clippy::doc_markdown)] // shell examples, not Rust identifiers
    SetKey(SetKeyArgs),
    /// Show which providers have a key and where it comes from.
    Status,
}

#[derive(Debug, Args)]
pub struct SetKeyArgs {
    /// Provider name.
    #[arg(long, default_value = "anthropic")]
    provider: String,
    /// Read the key from stdin (first line) instead of prompting.
    #[arg(long)]
    stdin: bool,
}

pub fn run(ctx: &Ctx, cmd: &AuthCommand) -> anyhow::Result<()> {
    match cmd {
        AuthCommand::SetKey(a) => {
            let key = read_key(a)?;
            let provider = a.provider.clone();
            let params = AuthSetKeyParams {
                provider: provider.clone(),
                key,
            };
            let status = with_client(ctx, |c| async move {
                c.call::<AuthSetKey>(params).await?;
                Ok(c.call::<AuthStatus>(Empty {}).await?)
            })?;
            let entry = status.providers.iter().find(|p| p.name == provider);
            let source = entry.and_then(|p| p.source.clone());
            if ctx.out.json {
                ctx.out.emit_json(&serde_json::json!({
                    "provider": provider,
                    "configured": entry.is_some_and(|p| p.configured),
                    "source": source,
                }))?;
            } else {
                ctx.out.line(format!(
                    "stored key for {provider} ({})",
                    source.as_deref().unwrap_or("stored")
                ));
            }
        }
        AuthCommand::Status => {
            let status = with_client(
                ctx,
                |c| async move { Ok(c.call::<AuthStatus>(Empty {}).await?) },
            )?;
            if ctx.out.json {
                ctx.out.emit_json(&status)?;
            } else {
                let rows: Vec<Vec<String>> = status
                    .providers
                    .iter()
                    .map(|p| {
                        vec![
                            p.name.clone(),
                            if p.configured {
                                "configured".into()
                            } else {
                                "not configured".into()
                            },
                            p.source.clone().unwrap_or_default(),
                        ]
                    })
                    .collect();
                ctx.out
                    .print_table(&["provider", "status", "source"], &rows);
            }
        }
    }
    Ok(())
}

/// The key from stdin (`--stdin`, or a pipe) or a hidden prompt.
fn read_key(a: &SetKeyArgs) -> anyhow::Result<String> {
    let key = if a.stdin || !std::io::stdin().is_terminal() {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .context("reading the key from stdin")?;
        text.lines().next().unwrap_or_default().trim().to_owned()
    } else {
        rpassword::prompt_password(format!("{} API key: ", a.provider))
            .context("reading the key from the terminal")?
            .trim()
            .to_owned()
    };
    if key.is_empty() {
        return Err(crate::Exit::Usage("no key given".into()).into());
    }
    Ok(key)
}
