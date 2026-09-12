//! `write_file`: create or replace a whole file, atomically, with a diff
//! of what it replaced in the trace.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::atomic::write_atomic;
use super::diff::unified_diff;
use super::text::{count_lines, decode, is_binary};
use super::{Target, blocking, parse, plural, plus_minus, target};
use crate::tools::{Risk, SeenFiles, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};
use crate::trace::sha256_hex;
use crate::workspace::Workspace;

/// Writes a whole file.
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteFile;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    path: String,
    content: String,
}

const DESCRIPTION: &str = "Create or overwrite a file in the workspace with exactly `content` (UTF-8, line \
endings as given). Missing parent directories are created; the write is atomic. To change part of an \
existing file prefer edit_file. Read a file before overwriting it: the result notes when you replace a \
file you have not read, or one that changed since you read it, and includes a unified diff of what \
changed.";

#[async_trait]
impl Tool for WriteFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "write_file",
            DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "minLength": 1,
                        "description": "File path relative to the workspace root."
                    },
                    "content": {
                        "type": "string",
                        "description": "The complete new content of the file."
                    }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            Risk::Write,
        )
        .with_tags(["file"])
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        input: Value,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let Input { path, content } = parse::<Input>(input)?;
        let (ws, t) = target(ctx, &path)?;
        let seen = ctx.seen.clone();
        blocking(move || Ok(write(&ws, &t, &content, &seen))).await
    }
}

/// The note a write or an edit carries about what it replaced.
pub(crate) fn overwrite_note(
    seen: &SeenFiles,
    abs: &Path,
    previous: &[u8],
) -> Option<&'static str> {
    match seen.hash_of(abs) {
        None => Some("note: overwrote a file you had not read"),
        Some(hash) if hash != sha256_hex(previous) => {
            Some("note: the file changed on disk since you read it")
        }
        Some(_) => None,
    }
}

fn write(ws: &Workspace, t: &Target, content: &str, seen: &SeenFiles) -> ToolOutput {
    let shown = &t.shown;
    if t.abs.is_dir() {
        return ToolOutput::error(format!("`{shown}` is a directory"));
    }
    let previous = match std::fs::read(&t.abs) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return ToolOutput::error(format!("cannot read the existing `{shown}`: {e}")),
    };
    let note = previous
        .as_deref()
        .and_then(|prev| overwrite_note(seen, &t.abs, prev));
    let diff = previous.as_deref().map(|prev| {
        if is_binary(prev) {
            None
        } else {
            Some(unified_diff(shown, &decode(prev).text, content))
        }
    });
    if let Err(e) = write_atomic(&t.abs, content.as_bytes()) {
        return ToolOutput::error(format!("cannot write `{shown}`: {e}"));
    }
    seen.record(&t.abs, sha256_hex(content.as_bytes()));
    let created = previous.is_none();
    if created {
        ws.invalidate_index();
    }

    let bytes = content.len();
    let (added, removed) = match &diff {
        Some(Some(d)) => (d.added, d.removed),
        // Everything is new, or the old content was binary: count what
        // was written.
        _ => (count_lines(content), 0),
    };
    let (mut out, summary) = if created {
        let lines = plural(added, "line", "lines");
        (
            format!("wrote {shown} ({bytes} bytes, new file, {lines})\n"),
            format!("wrote {shown} (new, {lines})"),
        )
    } else {
        let change = plus_minus(added, removed);
        (
            format!("wrote {shown} ({bytes} bytes, {change})\n"),
            format!("wrote {shown} ({change})"),
        )
    };
    if let Some(note) = note {
        out.push_str(note);
        out.push('\n');
    }
    match &diff {
        Some(Some(d)) if !d.text.is_empty() => {
            out.push('\n');
            out.push_str(&d.text);
        }
        Some(Some(_)) => out.push_str("(content unchanged)\n"),
        Some(None) => out.push_str("(previous content was binary; no diff)\n"),
        None => {}
    }
    let mut metadata = json!({
        "bytes_written": bytes,
        "created": created,
        "added": added,
        "removed": removed,
    });
    if let Some(note) = note {
        metadata["note"] = json!(note);
    }
    ToolOutput::text(out)
        .with_summary(summary)
        .with_metadata(metadata)
}
