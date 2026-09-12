//! `harness config path | get [KEY] | set KEY VALUE`.

use std::path::PathBuf;

use apprentice_api::methods::{
    ConfigGet, ConfigGetParams, ConfigPath, ConfigSet, ConfigSetParams, Empty,
};
use apprentice_api::types::ConfigLayer;
use clap::{Args, Subcommand};
use serde_json::Value;

use crate::Ctx;
use crate::daemon::with_client;

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the user config file and data directory.
    Path,
    /// Print the resolved config (TOML), or one dotted key with its source.
    ///
    /// Examples:
    ///   harness config get
    ///   harness config get mentor.model
    ///   harness config get --workspace ~/proj apprentice.enabled
    #[command(verbatim_doc_comment)]
    Get(GetArgs),
    /// Write one key to the user config, or to a workspace's `.harness/config.toml`.
    ///
    /// VALUE is parsed as JSON when it is one (`true`, `12`, `1.5`, `"x"`,
    /// `[1,2]`, `{"a":1}`), otherwise taken as a string; `null` removes the
    /// key.
    ///
    /// Examples:
    ///   harness config set mentor.model claude-opus-5
    ///   harness config set mentor.max_tokens 8000
    ///   harness config set --workspace . apprentice.enabled false
    #[command(verbatim_doc_comment)]
    #[allow(clippy::doc_markdown)] // shell examples, not Rust identifiers
    Set(SetArgs),
}

#[derive(Debug, Args)]
pub struct GetArgs {
    /// Dotted key, e.g. `mentor.model`. Omit for the whole config.
    key: Option<String>,
    /// Resolve with this workspace's overrides applied.
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct SetArgs {
    /// Dotted key, e.g. `mentor.model`.
    key: String,
    /// New value (JSON or plain string; `null` removes the key).
    value: String,
    /// Write to this workspace's `.harness/config.toml` instead of the user file.
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,
}

pub fn run(ctx: &Ctx, cmd: &ConfigCommand) -> anyhow::Result<()> {
    match cmd {
        ConfigCommand::Path => {
            let r = with_client(
                ctx,
                |c| async move { Ok(c.call::<ConfigPath>(Empty {}).await?) },
            )?;
            if ctx.out.json {
                ctx.out.emit_json(&r)?;
            } else {
                ctx.out
                    .print_kv(&[("config file", &r.config_file), ("data dir", &r.data_dir)]);
            }
        }
        ConfigCommand::Get(a) => {
            let params = ConfigGetParams {
                key: a.key.clone(),
                workspace: a.workspace.as_ref().map(workspace_string).transpose()?,
            };
            let r = with_client(
                ctx,
                |c| async move { Ok(c.call::<ConfigGet>(params).await?) },
            )?;
            if ctx.out.json {
                ctx.out.emit_json(&r)?;
            } else {
                ctx.out.line(render_value(&r.value));
                if let Some(source) = r.source {
                    let source = serde_json::to_value(source)?;
                    ctx.out
                        .info(format!("# from {}", source.as_str().unwrap_or("?")));
                }
            }
        }
        ConfigCommand::Set(a) => {
            let params = ConfigSetParams {
                key: a.key.clone(),
                value: parse_value(&a.value),
                layer: if a.workspace.is_some() {
                    ConfigLayer::Workspace
                } else {
                    ConfigLayer::User
                },
                workspace: a.workspace.as_ref().map(workspace_string).transpose()?,
            };
            let key = params.key.clone();
            let removed = params.value.is_null();
            with_client(
                ctx,
                |c| async move { Ok(c.call::<ConfigSet>(params).await?) },
            )?;
            if ctx.out.json {
                ctx.out
                    .emit_json(&serde_json::json!({ "key": key, "removed": removed }))?;
            } else if removed {
                ctx.out.info(format!("removed {key}"));
            } else {
                ctx.out.info(format!("set {key}"));
            }
        }
    }
    Ok(())
}

/// Absolute workspace path as a string for the wire (the daemon may have a
/// different working directory).
pub fn workspace_string(dir: &PathBuf) -> anyhow::Result<String> {
    let abs = std::path::absolute(dir)?;
    Ok(abs.display().to_string())
}

/// JSON when it parses, a string otherwise.
pub fn parse_value(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned()))
}

/// Tables as TOML, scalars bare, strings without quotes.
pub fn render_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(_) => toml::to_string_pretty(v).map_or_else(
            |_| serde_json::to_string_pretty(v).unwrap_or_default(),
            |t| t.trim_end().to_owned(),
        ),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_parse_as_json_first() {
        assert_eq!(parse_value("true"), Value::Bool(true));
        assert_eq!(parse_value("12"), serde_json::json!(12));
        assert_eq!(parse_value("1.5"), serde_json::json!(1.5));
        assert_eq!(parse_value("null"), Value::Null);
        assert_eq!(parse_value("[1, 2]"), serde_json::json!([1, 2]));
        assert_eq!(
            parse_value("claude-opus-5"),
            Value::String("claude-opus-5".into())
        );
        assert_eq!(parse_value("\"quoted\""), Value::String("quoted".into()));
    }

    #[test]
    fn rendering_is_toml_for_tables_and_bare_for_scalars() {
        assert_eq!(render_value(&Value::String("x".into())), "x");
        assert_eq!(render_value(&serde_json::json!(12)), "12");
        let table = serde_json::json!({"mentor": {"model": "m", "max_tokens": 8}});
        let text = render_value(&table);
        assert!(text.contains("[mentor]"), "{text}");
        assert!(text.contains("model = \"m\""), "{text}");
    }
}
