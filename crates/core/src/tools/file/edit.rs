//! `edit_file`: exact string replacement with helpful failures.

use std::fmt::Write as _;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::atomic::write_atomic;
use super::diff::{closest_line, unified_diff};
use super::text::{LineEnding, decode, is_binary, line_of, sniff_type};
use super::write::overwrite_note;
use super::{Target, blocking, parse, plural, plus_minus, target};
use crate::tools::{Risk, SeenFiles, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};
use crate::trace::sha256_hex;

/// Replaces one (or every) occurrence of a string in a file.
#[derive(Debug, Clone, Copy, Default)]
pub struct EditFile;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    path: String,
    old_string: String,
    new_string: String,
    #[serde(default)]
    replace_all: bool,
}

const DESCRIPTION: &str = "Replace text in a file in the workspace. `old_string` must match the current \
content exactly — whitespace and indentation included; quote it as read_file shows it, without the \
line-number prefix — and must occur exactly once: include surrounding lines to make it unique, or set \
`replace_all` to change every occurrence. `new_string` takes its place (empty deletes it). Line \
endings and a byte-order mark are preserved. The result is a unified diff of the change; a failed \
match reports the closest line, an ambiguous one the line of every occurrence.";

/// How many occurrence line numbers an ambiguity error lists.
const MAX_LISTED: usize = 10;

#[async_trait]
impl Tool for EditFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "edit_file",
            DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "minLength": 1,
                        "description": "File path relative to the workspace root."
                    },
                    "old_string": {
                        "type": "string",
                        "minLength": 1,
                        "description": "The exact text to replace."
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The replacement text; empty to delete."
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Replace every occurrence instead of requiring exactly one. Default false."
                    }
                },
                "required": ["path", "old_string", "new_string"],
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
        let Input {
            path,
            old_string,
            new_string,
            replace_all,
        } = parse::<Input>(input)?;
        let (_ws, t) = target(ctx, &path)?;
        let seen = ctx.seen.clone();
        blocking(move || Ok(edit(&t, &old_string, &new_string, replace_all, &seen))).await
    }
}

fn edit(t: &Target, old: &str, new: &str, replace_all: bool, seen: &SeenFiles) -> ToolOutput {
    let shown = &t.shown;
    if old == new {
        return ToolOutput::error("old_string and new_string are identical; nothing to do");
    }
    if t.abs.is_dir() {
        return ToolOutput::error(format!("`{shown}` is a directory"));
    }
    let bytes = match std::fs::read(&t.abs) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ToolOutput::error(format!(
                "`{shown}` does not exist; use write_file to create it"
            ));
        }
        Err(e) => return ToolOutput::error(format!("cannot read `{shown}`: {e}")),
    };
    if is_binary(&bytes) {
        return ToolOutput::error(format!(
            "`{shown}` is a binary file ({} bytes, {}); edit_file works on text",
            bytes.len(),
            sniff_type(&bytes, &t.abs)
        ));
    }
    let decoded = decode(&bytes);
    if !decoded.encoding.is_exact_utf8() {
        return ToolOutput::error(format!(
            "`{shown}` is {}, which edit_file cannot rewrite faithfully; use write_file",
            match decoded.encoding.label() {
                "utf-8-lossy" => "not valid UTF-8".to_owned(),
                other => other.to_uppercase(),
            }
        ));
    }
    let note = overwrite_note(seen, &t.abs, &bytes);

    // A CRLF file is matched and edited as LF when the mentor quoted it
    // without `\r` (read_file shows none), then converted back whole:
    // untouched lines come out byte-identical because none had a bare
    // `\n`. Mixed files are edited exactly as they are.
    let crlf = decoded.eol == LineEnding::CrLf && !old.contains('\r');
    let (haystack, old, new) = if crlf {
        (
            decoded.text.replace("\r\n", "\n"),
            old.to_owned(),
            new.replace("\r\n", "\n"),
        )
    } else {
        (decoded.text.clone(), old.to_owned(), new.to_owned())
    };

    let positions: Vec<usize> = haystack
        .match_indices(old.as_str())
        .map(|(i, _)| i)
        .collect();
    if positions.is_empty() {
        let mut msg = format!("old_string not found in `{shown}`.");
        match closest_line(
            old.lines().find(|l| !l.trim().is_empty()).unwrap_or(&old),
            &haystack,
        ) {
            Some((n, line)) => {
                let _ = write!(msg, "\nClosest line ({n}): {line}");
            }
            None => msg.push_str("\nNo similar line either."),
        }
        msg.push_str(
            "\nQuote the text exactly as read_file shows it (without the line-number prefix), \
             including whitespace.",
        );
        return ToolOutput::error(msg);
    }
    if positions.len() > 1 && !replace_all {
        let mut lines: Vec<usize> = positions.iter().map(|&i| line_of(&haystack, i)).collect();
        lines.dedup();
        let more = if lines.len() > MAX_LISTED {
            format!(", … (+{} more)", lines.len() - MAX_LISTED)
        } else {
            String::new()
        };
        let lines: Vec<String> = lines
            .iter()
            .take(MAX_LISTED)
            .map(usize::to_string)
            .collect();
        return ToolOutput::error(format!(
            "old_string occurs {} times in `{shown}` (lines {}{more}); include more surrounding \
             context so it matches once, or set replace_all: true",
            positions.len(),
            lines.join(", ")
        ));
    }
    let replacements = if replace_all { positions.len() } else { 1 };
    let edited = if replace_all {
        haystack.replace(old.as_str(), &new)
    } else {
        haystack.replacen(old.as_str(), &new, 1)
    };
    let diff = unified_diff(shown, &haystack, &edited);
    let final_text = if crlf {
        edited.replace('\n', "\r\n")
    } else {
        edited
    };
    let mut out_bytes = decoded.bom.bytes().to_vec();
    out_bytes.extend_from_slice(final_text.as_bytes());
    if let Err(e) = write_atomic(&t.abs, &out_bytes) {
        return ToolOutput::error(format!("cannot write `{shown}`: {e}"));
    }
    seen.record(&t.abs, sha256_hex(&out_bytes));

    let mut out = format!(
        "edited {shown} ({}, {})\n",
        plus_minus(diff.added, diff.removed),
        plural(replacements, "replacement", "replacements")
    );
    if let Some(note) = note {
        out.push_str(note);
        out.push('\n');
    }
    out.push('\n');
    out.push_str(&diff.text);
    let mut metadata = json!({
        "replacements": replacements,
        "added": diff.added,
        "removed": diff.removed,
        "line_endings": decoded.eol.label(),
        "bom": decoded.bom != super::text::Bom::None,
    });
    if let Some(note) = note {
        metadata["note"] = json!(note);
    }
    ToolOutput::text(out)
        .with_summary(format!(
            "edited {shown} ({})",
            plus_minus(diff.added, diff.removed)
        ))
        .with_metadata(metadata)
}
