//! `glob`: files by name pattern, from the workspace index.

use std::fmt::Write as _;

use async_trait::async_trait;
use globset::GlobBuilder;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{GLOB_MAX_RESULTS, Target, blocking, parse, plural, target};
use crate::tools::{Risk, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};
use crate::workspace::Workspace;

/// Finds files by pattern.
#[derive(Debug, Clone, Copy, Default)]
pub struct Glob;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    pattern: String,
    path: Option<String>,
}

const DESCRIPTION: &str = "Find files in the workspace by name pattern, gitignore-style: `*` matches \
within one path segment, `**` across segments, `?` one character, `{a,b}` alternatives, `[abc]` a \
character class. A pattern without `/` matches file names at any depth (`*.rs`); one with `/` \
matches the path relative to `path` (`src/**/*.rs`). `path` defaults to the workspace root. The \
output is one workspace-relative path per line, most recently modified first, at most 1000 (a \
`[truncated: ...]` trailer says how many matched). Ignored files are not searched.";

#[async_trait]
impl Tool for Glob {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "glob",
            DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "minLength": 1,
                        "description": "The glob pattern, e.g. `*.rs`, `src/**/*.test.ts`, `**/Cargo.toml`."
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory to search, relative to the workspace root. Default: the root."
                    }
                },
                "required": ["pattern"],
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
        let Input { pattern, path } = parse::<Input>(input)?;
        let (ws, t) = target(ctx, path.as_deref().unwrap_or("."))?;
        let matcher = GlobBuilder::new(pattern.trim_start_matches('/'))
            .literal_separator(true)
            .build()
            .map_err(|e| ToolError::InvalidInput(format!("bad glob pattern: {e}")))?
            .compile_matcher();
        let by_name = !pattern.trim_start_matches('/').contains('/');
        blocking(move || Ok(glob(&ws, &t, &pattern, &matcher, by_name))).await
    }
}

fn glob(
    ws: &Workspace,
    t: &Target,
    pattern: &str,
    matcher: &globset::GlobMatcher,
    by_name: bool,
) -> ToolOutput {
    let shown = &t.shown;
    match std::fs::metadata(&t.abs) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return ToolOutput::error(format!("`{shown}` is a file, not a directory")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ToolOutput::error(format!("`{shown}` does not exist"));
        }
        Err(e) => return ToolOutput::error(format!("cannot search `{shown}`: {e}")),
    }
    let index = ws.index();
    let dir = if shown == "." { "" } else { shown.as_str() };
    let prefix_len = if dir.is_empty() { 0 } else { dir.len() + 1 };
    let mut found: Vec<(&str, u64)> = index
        .under(dir)
        .filter(|e| {
            let rel = &e.path[prefix_len..];
            let subject = if by_name {
                rel.rsplit('/').next().unwrap_or(rel)
            } else {
                rel
            };
            matcher.is_match(subject)
        })
        .map(|e| (e.path.as_str(), e.mtime.unwrap_or(0)))
        .collect();
    found.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let total = found.len();
    let mut out = String::new();
    if index.is_truncated() {
        let _ = writeln!(
            out,
            "[note: the file index stopped at {} files; matches beyond it are missing]",
            index.len()
        );
    }
    if total == 0 {
        let _ = writeln!(out, "[no files match {pattern}]");
    }
    for (path, _) in found.iter().take(GLOB_MAX_RESULTS) {
        out.push_str(path);
        out.push('\n');
    }
    let truncated = total > GLOB_MAX_RESULTS;
    if truncated {
        let _ = writeln!(
            out,
            "[truncated: showing {GLOB_MAX_RESULTS} of {total} matches]"
        );
    }
    ToolOutput::text(out)
        .with_summary(format!(
            "glob {pattern} \u{2192} {}",
            plural(total, "file", "files")
        ))
        .with_metadata(json!({
            "matches": total,
            "shown": total.min(GLOB_MAX_RESULTS),
            "truncated": truncated,
            "index_age_s": index.age().as_secs(),
            "index_truncated": index.is_truncated(),
        }))
}
