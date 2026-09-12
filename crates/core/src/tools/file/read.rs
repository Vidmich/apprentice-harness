//! `read_file`: line-numbered text with paging.

use std::fmt::Write as _;
use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::text::{Encoding, decode, is_binary, sniff_type};
use super::{READ_MAX_BYTES, READ_MAX_LINES, Target, blocking, parse, target};
use crate::tools::{Risk, SeenFiles, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};
use crate::trace::sha256_hex;
use crate::workspace::language_of;

/// Reads a file as `<n>\t<line>`.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadFile;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

const DESCRIPTION: &str = "Read a text file in the workspace. The output has one line per file line as \
`<line number><TAB><text>`; line numbers are 1-based and absolute, so `offset` (first line to show) \
and `limit` (how many lines) page through long files. Defaults: from line 1, up to 2000 lines or \
200 KiB per call, after which a `[truncated: ...]` trailer says where to continue. Paths are \
relative to the workspace root with `/` separators. Binary files are reported, not shown. Use \
list_dir or glob to find files first.";

#[async_trait]
impl Tool for ReadFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "read_file",
            DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "minLength": 1,
                        "description": "File path relative to the workspace root."
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "First line to show (1-based). Default 1."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum number of lines to show. Default and maximum 2000."
                    }
                },
                "required": ["path"],
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
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let Input {
            path,
            offset,
            limit,
        } = parse::<Input>(input)?;
        let (_ws, t) = target(ctx, &path)?;
        let seen = ctx.seen.clone();
        blocking(move || Ok(read(&t, offset.unwrap_or(1), limit, &seen))).await
    }
}

fn read(t: &Target, offset: usize, limit: Option<usize>, seen: &SeenFiles) -> ToolOutput {
    let shown = &t.shown;
    if t.abs.is_dir() {
        return ToolOutput::error(format!("`{shown}` is a directory; use list_dir"));
    }
    let bytes = match std::fs::read(&t.abs) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ToolOutput::error(format!("`{shown}` does not exist"));
        }
        Err(e) => return ToolOutput::error(format!("cannot read `{shown}`: {e}")),
    };
    seen.record(&t.abs, sha256_hex(&bytes));
    if is_binary(&bytes) {
        return ToolOutput::error(format!(
            "`{shown}` is a binary file ({} bytes, {}); read_file shows text only",
            bytes.len(),
            sniff_type(&bytes, &t.abs)
        ))
        .with_metadata(json!({ "bytes": bytes.len(), "binary": true }));
    }
    render(
        t,
        &bytes,
        offset,
        limit.unwrap_or(READ_MAX_LINES).min(READ_MAX_LINES),
    )
}

/// The line-numbered view; separate so the goldens can pin it down.
pub(crate) fn render(t: &Target, bytes: &[u8], offset: usize, limit: usize) -> ToolOutput {
    let shown = &t.shown;
    let decoded = decode(bytes);
    let language = language_of(Path::new(shown));
    let lines: Vec<&str> = decoded
        .text
        .split_inclusive('\n')
        .map(|l| l.trim_end_matches('\n').trim_end_matches('\r'))
        .collect();
    let total = lines.len();
    let mut metadata = json!({
        "lines": total,
        "bytes": bytes.len(),
        "language": language,
        "truncated": false,
        "encoding": decoded.encoding.label(),
        "bom": decoded.bom != super::text::Bom::None,
        "line_endings": decoded.eol.label(),
    });
    let mut notes = String::new();
    match decoded.encoding {
        Encoding::Utf8 => {}
        Encoding::Utf8Lossy => {
            notes.push_str("[note: not valid UTF-8; undecodable bytes shown as U+FFFD]\n");
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let _ = writeln!(
                notes,
                "[note: decoded from {}; edit_file cannot change this file]",
                decoded.encoding.label().to_uppercase()
            );
        }
    }
    if total == 0 {
        return ToolOutput::text(format!("{notes}[empty file]"))
            .with_summary(format!("read {shown} (empty)"))
            .with_metadata(metadata);
    }
    if offset > total {
        return ToolOutput::error(format!(
            "offset {offset} is past the end of `{shown}` ({})",
            super::plural(total, "line", "lines")
        ))
        .with_metadata(metadata);
    }
    let first = offset;
    let mut out = notes;
    let mut emitted = 0usize;
    let mut content_bytes = 0usize;
    for (i, line) in lines.iter().enumerate().skip(first - 1) {
        let n = i + 1;
        let cost = line.len() + 1;
        if emitted == limit || (emitted > 0 && content_bytes + cost > READ_MAX_BYTES) {
            break;
        }
        out.push_str(&n.to_string());
        out.push('\t');
        out.push_str(line);
        out.push('\n');
        emitted += 1;
        content_bytes += cost;
    }
    let last = first + emitted - 1;
    let truncated = last < total;
    if truncated {
        let _ = writeln!(
            out,
            "[truncated: showing lines {first}\u{2013}{last} of {total}; call again with offset {}]",
            last + 1
        );
    }
    metadata["truncated"] = json!(truncated);
    metadata["offset"] = json!(first);
    metadata["end"] = json!(last);
    ToolOutput::text(out)
        .with_summary(format!(
            "read {shown} lines {first}\u{2013}{last} of {total}"
        ))
        .with_metadata(metadata)
}
