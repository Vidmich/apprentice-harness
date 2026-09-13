//! The search tool (task M01-04): `grep`, a regex search over the
//! workspace on the ripgrep libraries (`grep-regex`, `grep-searcher`)
//! and the workspace's ignore-aware walker, so results are the same on
//! every platform without an `rg` binary.
//!
//! Three output modes, all sorted by path (byte order of the
//! `/`-separated workspace-relative form) so the raw blob is regular
//! enough for the compressor (M03) to learn:
//! - `content`: `path:line:col:text` per matching line, context lines
//!   as `path-line-text`, `--` between non-adjacent groups;
//! - `files`: one path per line;
//! - `count`: `path: N` per file, most matches first.
//!
//! Every mode ends with a total (`[N matches in M files]`, `[N files]`,
//! or `[no matches]`) and, when `max_results` cut the output, the
//! trailer `[limit reached; narrow the pattern or path]`.

mod search;
#[cfg(test)]
mod tests;

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use self::search::{Line, Options, SearchResult, run};
use super::file::{PathGlob, blocking, parse, plural, target};
use crate::tools::{Risk, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};

/// `max_results` when the mentor does not say.
pub const GREP_DEFAULT_RESULTS: usize = 200;
/// The most `max_results` may ask for.
pub const GREP_MAX_RESULTS: usize = 1000;
/// The most context lines on each side of a match.
pub const GREP_MAX_CONTEXT: usize = 10;
/// Matching and context lines are cut here (characters) with `…`.
pub const GREP_LINE_CHARS: usize = 400;

/// The search tool, ready for [`crate::tools::ToolRegistry::register`].
pub fn search_tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(Grep)]
}

/// Searches file contents with a regex.
#[derive(Debug, Clone, Copy, Default)]
pub struct Grep;

/// What the output lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Matching paths only.
    Files,
    /// Matching lines, with context.
    #[default]
    Content,
    /// Matches per file.
    Count,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Content => "content",
            Self::Count => "count",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    #[serde(default)]
    case_insensitive: bool,
    #[serde(default)]
    mode: Mode,
    #[serde(default)]
    context: u64,
    max_results: Option<u64>,
    #[serde(default)]
    multiline: bool,
}

const DESCRIPTION: &str = "Search file contents in the workspace with a regular expression (Rust \
regex syntax). Ignored files (`.gitignore`, `.harness/ignore`, VCS and build directories, \
binaries) are skipped unless `path` names a file directly. `mode`: `content` (default) prints \
`path:line:col:text` for each matching line, with `context` lines before and after as \
`path-line-text` and `--` between groups; `files` prints one matching path per line; `count` \
prints `path: N` per file, most matches first. `glob` narrows the search to matching file names \
(`*.rs`) or paths relative to `path` (`src/**/*.ts`). `case_insensitive` ignores case. `multiline` \
lets a pattern span lines (`.` then also matches a newline); otherwise a pattern that could match \
a newline is an error. Lines longer than 400 characters are cut with `…`. Output stops at \
`max_results` (default 200, at most 1000) matches or files with a `[limit reached ...]` trailer: \
narrow the pattern, the `glob` or the `path` then. Paths are workspace-relative; results are \
sorted by path, then line.";

#[async_trait]
impl Tool for Grep {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "grep",
            DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "minLength": 1,
                        "description": "The regular expression, e.g. `fn \\w+\\(`, `TODO|FIXME`, `^use std::`."
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search, relative to the workspace root. Default: the root."
                    },
                    "glob": {
                        "type": "string",
                        "description": "Only search files matching this glob, e.g. `*.rs`, `src/**/*.ts`."
                    },
                    "case_insensitive": {
                        "type": "boolean",
                        "description": "Ignore case. Default false."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["content", "files", "count"],
                        "description": "What to print: matching lines (`content`, default), matching paths (`files`), or matches per file (`count`)."
                    },
                    "context": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": GREP_MAX_CONTEXT,
                        "description": "Lines of context before and after each match (`content` mode, 0–10). Default 0."
                    },
                    "max_results": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": GREP_MAX_RESULTS,
                        "description": "Stop after this many matching lines (`content`) or files (`files`, `count`). Default 200, at most 1000."
                    },
                    "multiline": {
                        "type": "boolean",
                        "description": "Let the pattern span lines; `.` matches newlines too. Default false."
                    }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
            Risk::ReadOnly,
        )
        .with_tags(["search"])
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let Input {
            pattern,
            path,
            glob,
            case_insensitive,
            mode,
            context,
            max_results,
            multiline,
        } = parse::<Input>(input)?;
        let (ws, t) = target(ctx, path.as_deref().unwrap_or("."))?;
        let glob = glob.as_deref().map(PathGlob::compile).transpose()?;
        let options = Options {
            mode,
            case_insensitive,
            multiline,
            context: usize::try_from(context.min(GREP_MAX_CONTEXT as u64)).unwrap_or(0),
            limit: usize::try_from(
                max_results
                    .unwrap_or(GREP_DEFAULT_RESULTS as u64)
                    .clamp(1, GREP_MAX_RESULTS as u64),
            )
            .unwrap_or(GREP_DEFAULT_RESULTS),
            glob,
        };
        blocking(move || {
            let result = match run(&ws, &t, &pattern, &options, &cancel) {
                Ok(r) => r,
                Err(search::SearchError::Regex(msg)) => {
                    return Ok(ToolOutput::error(format!("invalid regex: {msg}")));
                }
                Err(search::SearchError::Path(msg)) => return Ok(ToolOutput::error(msg)),
                Err(search::SearchError::Cancelled) => return Err(ToolError::Cancelled),
            };
            Ok(render(&pattern, &options, &result))
        })
        .await
    }
}

/// The output text, summary and metadata for `result`.
fn render(pattern: &str, options: &Options, result: &SearchResult) -> ToolOutput {
    let mut out = String::new();
    let (shown, truncated) = match options.mode {
        Mode::Content => render_content(&mut out, options, result),
        Mode::Files => render_files(&mut out, options, result),
        Mode::Count => render_count(&mut out, options, result),
    };
    let files = result.files.len();
    let matches = matches_total(result);
    if files == 0 {
        out.push_str("[no matches]\n");
    } else if options.mode == Mode::Files {
        let _ = writeln!(out, "[{}]", plural(files, "file", "files"));
    } else {
        let _ = writeln!(
            out,
            "[{} in {}]",
            plural(matches, "match", "matches"),
            plural(files, "file", "files")
        );
    }
    if truncated {
        out.push_str("[limit reached; narrow the pattern or path]\n");
    }
    let summary = match options.mode {
        Mode::Files => format!(
            "grep {} \u{2192} {}",
            quote(pattern),
            plural(files, "file", "files")
        ),
        Mode::Content | Mode::Count => format!(
            "grep {} \u{2192} {} in {}",
            quote(pattern),
            plural(matches, "match", "matches"),
            plural(files, "file", "files")
        ),
    };
    ToolOutput::text(out)
        .with_summary(summary)
        .with_metadata(json!({
            "mode": options.mode.label(),
            // `files` stops at a file's first match: no count there.
            "matches": (options.mode != Mode::Files).then_some(matches),
            "files": files,
            "searched": result.searched,
            "shown": shown,
            "truncated": truncated,
            "overflowed": result.overflowed,
        }))
}

/// Matching lines with context; returns (matches shown, cut short).
fn render_content(out: &mut String, options: &Options, result: &SearchResult) -> (usize, bool) {
    let mut shown = 0usize;
    let mut printed_file = false;
    for file in &result.files {
        if options.context > 0 && printed_file {
            out.push_str("--\n");
        }
        printed_file = false;
        for line in &file.lines {
            match line {
                Line::Match { n, lines } => {
                    if shown == options.limit {
                        return (shown, true);
                    }
                    shown += 1;
                    for (n, (col, text)) in (*n..).zip(lines) {
                        let _ = writeln!(out, "{}:{n}:{col}:{text}", file.path);
                    }
                }
                Line::Context { n, text } => {
                    let _ = writeln!(out, "{}-{n}-{text}", file.path);
                }
                Line::Break => out.push_str("--\n"),
            }
            printed_file = true;
        }
        if file.partial {
            // The rest of this file (and maybe later ones) was counted
            // but not kept: stop rather than skip ahead.
            return (shown, true);
        }
    }
    (shown, shown < matches_total(result))
}

fn matches_total(result: &SearchResult) -> usize {
    result.files.iter().map(|f| f.matches).sum()
}

fn render_files(out: &mut String, options: &Options, result: &SearchResult) -> (usize, bool) {
    let shown = result.files.len().min(options.limit);
    for file in &result.files[..shown] {
        out.push_str(&file.path);
        out.push('\n');
    }
    (shown, shown < result.files.len())
}

fn render_count(out: &mut String, options: &Options, result: &SearchResult) -> (usize, bool) {
    let mut files: Vec<_> = result.files.iter().collect();
    files.sort_by(|a, b| b.matches.cmp(&a.matches).then_with(|| a.path.cmp(&b.path)));
    let shown = files.len().min(options.limit);
    for file in &files[..shown] {
        let _ = writeln!(out, "{}: {}", file.path, file.matches);
    }
    (shown, shown < files.len())
}

/// The pattern for the summary line: quoted and short.
fn quote(pattern: &str) -> String {
    const MAX: usize = 40;
    let mut s = String::from("\"");
    if pattern.chars().count() > MAX {
        s.extend(pattern.chars().take(MAX - 1));
        s.push('\u{2026}');
    } else {
        s.push_str(pattern);
    }
    s.push('"');
    s
}
