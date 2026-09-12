//! `list_dir`: an indented tree of a directory, ignore rules applied.

use std::fmt::Write as _;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{LIST_MAX_DEPTH, LIST_MAX_ENTRIES, Target, blocking, parse, plural, target};
use crate::tools::{Risk, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};
use crate::workspace::Workspace;

/// Lists a directory.
#[derive(Debug, Clone, Copy, Default)]
pub struct ListDir;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    path: Option<String>,
    depth: Option<u64>,
}

const DESCRIPTION: &str = "List a directory in the workspace as an indented tree: `name/` for \
directories, `name<TAB><size in bytes>` for files, `name@` for symlinks, two spaces of indent per \
level below `path`. `path` defaults to the workspace root; `depth` (1–4, default 1) is how many \
levels to descend. Ignored entries (`.gitignore`, `.harness/ignore`, VCS and build directories, \
binaries) are left out — read_file still opens them by exact path. Long listings stop at 2000 \
entries with a `[+N more]` trailer.";

#[async_trait]
impl Tool for ListDir {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "list_dir",
            DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path relative to the workspace root. Default: the root."
                    },
                    "depth": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": LIST_MAX_DEPTH,
                        "description": "Levels to descend (1–4). Default 1."
                    }
                },
                "additionalProperties": false
            }),
            Risk::ReadOnly,
        )
        .with_tags(["file"])
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let Input { path, depth } = parse::<Input>(input)?;
        let (ws, t) = target(ctx, path.as_deref().unwrap_or("."))?;
        let depth = usize::try_from(depth.unwrap_or(1).clamp(1, LIST_MAX_DEPTH)).unwrap_or(1);
        blocking(move || list(&ws, &t, depth, &cancel)).await
    }
}

fn list(
    ws: &Workspace,
    t: &Target,
    depth: usize,
    cancel: &CancellationToken,
) -> Result<ToolOutput, ToolError> {
    let shown = &t.shown;
    match std::fs::metadata(&t.abs) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            return Ok(ToolOutput::error(format!(
                "`{shown}` is a file; use read_file"
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ToolOutput::error(format!("`{shown}` does not exist")));
        }
        Err(e) => return Ok(ToolOutput::error(format!("cannot list `{shown}`: {e}"))),
    }
    let mut out = String::new();
    let mut total = 0usize;
    for (i, entry) in ws.walker_at(&t.abs, depth).enumerate() {
        if i % 256 == 0 && cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let entry = match entry {
            Ok(e) => e,
            Err(err) => {
                tracing::debug!(error = %err, "list_dir walk");
                continue;
            }
        };
        if entry.depth() == 0 {
            continue;
        }
        total += 1;
        if total > LIST_MAX_ENTRIES {
            continue;
        }
        for _ in 1..entry.depth() {
            out.push_str("  ");
        }
        let name = entry.file_name().to_string_lossy();
        let kind = entry.file_type();
        if kind.is_some_and(|k| k.is_symlink()) {
            out.push_str(&name);
            out.push('@');
        } else if kind.is_some_and(|k| k.is_dir()) {
            out.push_str(&name);
            out.push('/');
        } else {
            let size = entry.metadata().map_or(0, |m| m.len());
            out.push_str(&name);
            out.push('\t');
            out.push_str(&size.to_string());
        }
        out.push('\n');
    }
    let truncated = total > LIST_MAX_ENTRIES;
    if truncated {
        let _ = writeln!(out, "[+{} more]", total - LIST_MAX_ENTRIES);
    }
    if total == 0 {
        out.push_str("[empty directory]\n");
    }
    let dir = if shown == "." {
        ".".to_owned()
    } else {
        format!("{shown}/")
    };
    Ok(ToolOutput::text(out)
        .with_summary(format!(
            "listed {dir} ({})",
            plural(total, "entry", "entries")
        ))
        .with_metadata(json!({
            "entries": total,
            "shown": total.min(LIST_MAX_ENTRIES),
            "depth": depth,
            "truncated": truncated,
        })))
}
