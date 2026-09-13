//! The git tools (task M01-06): `git_status`, `git_diff` and `git_log`,
//! read-only views of the workspace's repository for the mentor. They
//! shell out to the user's `git` through [`Repo`] and render compact,
//! stable text; a workspace that is not a repository, or a machine
//! without git, gets an error result that says so instead of a
//! failure. Writes (commit, branch, stash) stay with the `shell` tool
//! under permissions: the harness never commits on its own.
//!
//! Output formats:
//! - `git_status`: a `branch` line, then `staged:` / `unstaged:` /
//!   `unmerged:` / `untracked:` groups with `  <X> <path>` lines
//!   (`R old -> new` for renames), or `nothing to commit, working tree
//!   clean`;
//! - `git_diff`: the unified diff, cut per file with
//!   `[diff truncated for <path>]` / `[diff omitted for <path>]`;
//! - `git_log`: `<hash> <date> <author> <subject>` per commit, or with
//!   `format: full` the whole message indented under that line.

mod diff;
#[cfg(test)]
mod tests;

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::file::{parse, plural, target};
use crate::tools::{Risk, Tool, ToolContext, ToolError, ToolOutput, ToolSpec};
use crate::workspace::git::{GitError, Repo, Status, parse_status};

/// `git_diff` output budget when the mentor does not say.
pub const DIFF_DEFAULT_BYTES: usize = 64 * 1024;
/// The most `max_bytes` may ask for.
pub const DIFF_MAX_BYTES: usize = 2 * 1024 * 1024;
/// `git_log` commits when the mentor does not say.
pub const LOG_DEFAULT_COUNT: u64 = 20;
/// The most `max_count` may ask for.
pub const LOG_MAX_COUNT: u64 = 200;
/// `git_status` lists at most this many paths per group.
pub const STATUS_MAX_ENTRIES: usize = 500;
/// Git output kept per call (the tools cut it further).
const STDOUT_MAX: usize = 8 * 1024 * 1024;

/// The three git tools, ready for [`crate::tools::ToolRegistry::register_all`].
pub fn git_tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(GitStatus), Arc::new(GitDiff), Arc::new(GitLog)]
}

/// The repository of the session's workspace, or the error result the
/// mentor should see instead.
async fn open_repo(ctx: &ToolContext) -> Result<Result<Repo, ToolOutput>, ToolError> {
    let (ws, _) = target(ctx, ".")?;
    Ok(Repo::open(ws.root()).await.map_err(|e| failed("git", &e)))
}

/// An error result for a git failure, with git's own words.
fn failed(what: &str, e: &GitError) -> ToolOutput {
    let summary = match e {
        GitError::NotAvailable => "git: not available".to_owned(),
        GitError::NotARepo { .. } => "git: not a repository".to_owned(),
        GitError::TimedOut { .. } => format!("{what}: timed out"),
        GitError::Failed { .. } | GitError::Io(_) => format!("{what}: failed"),
    };
    ToolOutput::error(format!("{e}\n"))
        .with_summary(summary)
        .with_metadata(json!({ "error": e.kind() }))
}

/// A `ref` argument must not look like an option.
fn check_ref(name: &str) -> Result<(), ToolError> {
    if name.is_empty() || name.starts_with('-') || name.contains(char::is_whitespace) {
        return Err(ToolError::InvalidInput(format!(
            "`{name}` is not a valid ref"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------- status

/// `git status` grouped by state.
#[derive(Debug, Clone, Copy, Default)]
pub struct GitStatus;

const STATUS_DESCRIPTION: &str = "Show the state of the workspace's git repository: the branch \
(with its upstream and how far ahead/behind), then the changed paths grouped as `staged:`, \
`unstaged:`, `unmerged:` and `untracked:` with git's one-letter status (`M` modified, `A` \
added, `D` deleted, `R old -> new` renamed, `T` type change, `UU` both modified). A clean tree \
prints `nothing to commit, working tree clean`. Paths are relative to the workspace root; \
changes elsewhere in the repository are counted, not listed. Read-only; use `shell` for git \
commands that change anything.";

#[async_trait]
impl Tool for GitStatus {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "git_status",
            STATUS_DESCRIPTION,
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            Risk::ReadOnly,
        )
        .with_tags(["git"])
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        _input: Value,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let repo = match open_repo(ctx).await? {
            Ok(repo) => repo,
            Err(out) => return Ok(out),
        };
        let out = match repo
            .run(&["status", "--porcelain=v2", "-z", "--branch"], STDOUT_MAX)
            .await
        {
            Ok(out) => out,
            Err(e) => return Ok(failed("git status", &e)),
        };
        if out.truncated {
            return Ok(ToolOutput::error(
                "git status output is too large to show; narrow the repository's ignore rules\n",
            )
            .with_summary("git status: too large"));
        }
        let mut status = parse_status(&out.stdout);
        let outside = status.restrict(repo.prefix());
        Ok(render_status(&status, outside))
    }
}

fn render_status(status: &Status, outside: usize) -> ToolOutput {
    let mut text = String::new();
    match (&status.branch, &status.head) {
        (Some(branch), Some(_)) => {
            let _ = write!(text, "branch {branch}");
            if let Some(up) = &status.upstream {
                let _ = write!(text, "...{up}");
                match (status.ahead, status.behind) {
                    (0, 0) => {}
                    (a, 0) => {
                        let _ = write!(text, " [ahead {a}]");
                    }
                    (0, b) => {
                        let _ = write!(text, " [behind {b}]");
                    }
                    (a, b) => {
                        let _ = write!(text, " [ahead {a}, behind {b}]");
                    }
                }
            }
            text.push('\n');
        }
        (Some(branch), None) => {
            let _ = writeln!(text, "branch {branch} (no commits yet)");
        }
        (None, Some(head)) => {
            let _ = writeln!(text, "HEAD detached at {}", &head[..head.len().min(12)]);
        }
        (None, None) => text.push_str("HEAD unborn\n"),
    }
    let mut group = |title: &str, entries: Vec<String>| {
        if entries.is_empty() {
            return;
        }
        let _ = writeln!(text, "{title}:");
        let n = entries.len();
        for line in entries.iter().take(STATUS_MAX_ENTRIES) {
            let _ = writeln!(text, "  {line}");
        }
        if n > STATUS_MAX_ENTRIES {
            let _ = writeln!(text, "  [... {} more]", n - STATUS_MAX_ENTRIES);
        }
    };
    let staged: Vec<String> = status
        .staged()
        .map(|e| match (&e.index, &e.orig_path) {
            ('R' | 'C', Some(orig)) => format!("{} {orig} -> {}", e.index, e.path),
            _ => format!("{} {}", e.index, e.path),
        })
        .collect();
    let unstaged: Vec<String> = status
        .unstaged()
        .map(|e| match (&e.worktree, &e.orig_path) {
            ('R' | 'C', Some(orig)) => format!("{} {orig} -> {}", e.worktree, e.path),
            _ => format!("{} {}", e.worktree, e.path),
        })
        .collect();
    let unmerged: Vec<String> = status
        .unmerged()
        .map(|e| format!("{}{} {}", e.index, e.worktree, e.path))
        .collect();
    let untracked: Vec<String> = status.untracked().map(|e| e.path.clone()).collect();
    let counts = (
        staged.len(),
        unstaged.len(),
        unmerged.len(),
        untracked.len(),
    );
    group("staged", staged);
    group("unstaged", unstaged);
    group("unmerged", unmerged);
    group("untracked", untracked);
    if status.is_clean() {
        text.push_str("nothing to commit, working tree clean\n");
    }
    if outside > 0 {
        let _ = writeln!(
            text,
            "[{} elsewhere in the repository not shown]",
            plural(outside, "change", "changes")
        );
    }

    let mut summary = format!(
        "git: {}",
        status
            .branch
            .as_deref()
            .unwrap_or(if status.head.is_some() {
                "detached"
            } else {
                "unborn"
            })
    );
    let (staged, unstaged, unmerged, untracked) = counts;
    let mut parts = Vec::new();
    if staged > 0 {
        parts.push(format!("{staged} staged"));
    }
    if unstaged > 0 {
        parts.push(format!("{unstaged} modified"));
    }
    if unmerged > 0 {
        parts.push(format!("{unmerged} unmerged"));
    }
    if untracked > 0 {
        parts.push(format!("{untracked} untracked"));
    }
    if parts.is_empty() {
        parts.push("clean".to_owned());
    }
    let _ = write!(summary, ", {}", parts.join(", "));

    ToolOutput::text(text)
        .with_summary(summary)
        .with_metadata(json!({
            "branch": status.branch,
            "upstream": status.upstream,
            "ahead": status.ahead,
            "behind": status.behind,
            "head": status.head,
            "staged": staged,
            "unstaged": unstaged,
            "unmerged": unmerged,
            "untracked": untracked,
            "clean": status.is_clean(),
            "outside": outside,
        }))
}

// ------------------------------------------------------------------ diff

/// `git diff` cut per file.
#[derive(Debug, Clone, Copy, Default)]
pub struct GitDiff;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiffInput {
    path: Option<String>,
    #[serde(default)]
    staged: bool,
    base: Option<String>,
    max_bytes: Option<u64>,
}

const DIFF_DESCRIPTION: &str = "Show uncommitted changes in the workspace as a unified diff \
(`git diff --no-color`). By default: the working tree against the index (unstaged changes); \
`staged: true` shows the index against `HEAD` (what a commit would contain); `base` compares \
against that ref instead (`HEAD`, `main`, `HEAD~3`, a hash) — with `staged` the index, \
otherwise the working tree. `path` restricts the diff to a file or directory (workspace-relative). \
Output stops at `max_bytes` (default 64 KiB, at most 2 MiB), cut between hunks with \
`[diff truncated for <path>]` and `[diff omitted for <path>]` lines: ask for a `path` then. \
`[no changes]` when nothing differs. Read-only.";

#[async_trait]
impl Tool for GitDiff {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "git_diff",
            DIFF_DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File or directory to diff, relative to the workspace root. Default: everything."
                    },
                    "staged": {
                        "type": "boolean",
                        "description": "Diff the index (staged changes) instead of the working tree. Default false."
                    },
                    "base": {
                        "type": "string",
                        "minLength": 1,
                        "pattern": "^[^-\\s][^\\s]*$",
                        "description": "Compare against this ref (`HEAD`, `main`, `HEAD~2`, a hash) instead of the index."
                    },
                    "max_bytes": {
                        "type": "integer",
                        "minimum": 1024,
                        "maximum": DIFF_MAX_BYTES,
                        "description": "Output budget in bytes. Default 65536, at most 2097152."
                    }
                },
                "additionalProperties": false
            }),
            Risk::ReadOnly,
        )
        .with_tags(["git"])
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        input: Value,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let input: DiffInput = parse(input)?;
        let pathspec = target(ctx, input.path.as_deref().unwrap_or("."))?.1.shown;
        if let Some(base) = &input.base {
            check_ref(base)?;
        }
        let max_bytes = input
            .max_bytes
            .map_or(DIFF_DEFAULT_BYTES, |n| {
                usize::try_from(n).unwrap_or(DIFF_MAX_BYTES)
            })
            .min(DIFF_MAX_BYTES);
        let repo = match open_repo(ctx).await? {
            Ok(repo) => repo,
            Err(out) => return Ok(out),
        };
        let mut args = vec!["diff", "--no-color", "--no-ext-diff", "--relative"];
        if input.staged {
            args.push("--cached");
        }
        if let Some(base) = &input.base {
            args.push(base);
        }
        args.push("--");
        args.push(&pathspec);
        let out = match repo.run(&args, STDOUT_MAX).await {
            Ok(out) => out,
            Err(e) => return Ok(failed("git diff", &e)),
        };
        let what = match (input.staged, &input.base) {
            (false, None) => "unstaged".to_owned(),
            (true, None) => "staged".to_owned(),
            (false, Some(base)) => format!("working tree vs {base}"),
            (true, Some(base)) => format!("index vs {base}"),
        };
        let metadata = json!({
            "path": (pathspec != ".").then_some(&pathspec),
            "staged": input.staged,
            "base": input.base,
        });
        if out.stdout.is_empty() {
            return Ok(ToolOutput::text("[no changes]\n")
                .with_summary(format!("git diff: no {what} changes"))
                .with_metadata(
                    json!({ "files": 0, "added": 0, "removed": 0, "truncated": false })
                        .merged(metadata),
                ));
        }
        let text = out.text();
        let cut = diff::cut(&text, max_bytes);
        let ge = if out.truncated { "\u{2265}" } else { "" };
        let mut summary = format!(
            "git diff: {ge}{}, +{ge}{} \u{2212}{ge}{}",
            plural(cut.files, "file", "files"),
            cut.added,
            cut.removed
        );
        if cut.truncated {
            summary.push_str(" (truncated)");
        }
        Ok(ToolOutput::text(cut.text)
            .with_summary(summary)
            .with_metadata(
                json!({
                    "files": cut.files,
                    "added": cut.added,
                    "removed": cut.removed,
                    "truncated": cut.truncated,
                    "partial": out.truncated,
                })
                .merged(metadata),
            ))
    }
}

/// `serde_json::Value` object merge for building metadata in two parts.
trait Merged {
    fn merged(self, other: Value) -> Value;
}

impl Merged for Value {
    fn merged(mut self, other: Value) -> Value {
        if let (Value::Object(a), Value::Object(b)) = (&mut self, other) {
            a.extend(b);
        }
        self
    }
}

// ------------------------------------------------------------------- log

/// `git log`, one line per commit or the full message.
#[derive(Debug, Clone, Copy, Default)]
pub struct GitLog;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum LogFormat {
    #[default]
    Oneline,
    Full,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LogInput {
    max_count: Option<u64>,
    path: Option<String>,
    #[serde(default)]
    format: LogFormat,
}

const LOG_DESCRIPTION: &str = "Show recent commits of the workspace's repository, newest first, \
as `<short hash> <date> <author> <subject>` per line (`format: oneline`, default) or with the \
whole commit message indented under that line (`format: full`). `path` limits the history to \
commits touching a file or directory (workspace-relative). At most `max_count` commits (default \
20, at most 200); a `[more commits ...]` trailer says when the history goes on. Read-only.";

#[async_trait]
impl Tool for GitLog {
    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "git_log",
            LOG_DESCRIPTION,
            json!({
                "type": "object",
                "properties": {
                    "max_count": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": LOG_MAX_COUNT,
                        "description": "How many commits at most. Default 20, at most 200."
                    },
                    "path": {
                        "type": "string",
                        "description": "Only commits touching this file or directory, relative to the workspace root."
                    },
                    "format": {
                        "type": "string",
                        "enum": ["oneline", "full"],
                        "description": "`oneline` (default): hash, date, author and subject; `full`: the whole message too."
                    }
                },
                "additionalProperties": false
            }),
            Risk::ReadOnly,
        )
        .with_tags(["git"])
    }

    async fn call(
        &self,
        ctx: &ToolContext,
        input: Value,
        _cancel: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        let input: LogInput = parse(input)?;
        let pathspec = match &input.path {
            Some(path) => Some(target(ctx, path)?.1.shown),
            None => None,
        };
        let max = input
            .max_count
            .unwrap_or(LOG_DEFAULT_COUNT)
            .clamp(1, LOG_MAX_COUNT);
        let repo = match open_repo(ctx).await? {
            Ok(repo) => repo,
            Err(out) => return Ok(out),
        };
        let format = match input.format {
            LogFormat::Oneline => "--format=%h %ad %an %s",
            LogFormat::Full => "--format=%h %ad %an%n%w(0,4,4)%B",
        };
        let count = format!("-n{}", max + 1);
        let mut args = vec!["log", "-z", "--no-color", "--date=short", format, &count];
        if let Some(p) = &pathspec {
            args.push("--");
            args.push(p);
        }
        let out = match repo.run(&args, STDOUT_MAX).await {
            Ok(out) => out,
            Err(GitError::Failed { stderr, .. })
                if stderr.contains("does not have any commits") =>
            {
                return Ok(no_commits(pathspec.as_ref(), input.format));
            }
            Err(e) => return Ok(failed("git log", &e)),
        };
        let text = out.text();
        let mut commits: Vec<String> = text
            .split('\0')
            .map(|c| match input.format {
                LogFormat::Oneline => c.trim_end().to_owned(),
                // `%w` pads the body's blank lines with the indent.
                LogFormat::Full => c.lines().map(str::trim_end).collect::<Vec<_>>().join("\n"),
            })
            .filter(|c| !c.is_empty())
            .collect();
        if commits.is_empty() {
            return Ok(no_commits(pathspec.as_ref(), input.format));
        }
        let more = commits.len() > usize::try_from(max).unwrap_or(usize::MAX);
        if more {
            commits.pop();
        }
        let sep = match input.format {
            LogFormat::Oneline => "\n",
            LogFormat::Full => "\n\n",
        };
        let mut text = commits.join(sep);
        text.push('\n');
        if more {
            let _ = writeln!(text, "[more commits; raise max_count]");
        }
        let mut summary = format!("git log: {}", plural(commits.len(), "commit", "commits"));
        if let Some(p) = &pathspec {
            let _ = write!(summary, " touching {p}");
        }
        if more {
            summary.push_str(", more");
        }
        Ok(ToolOutput::text(text)
            .with_summary(summary)
            .with_metadata(json!({
                "count": commits.len(),
                "more": more,
                "path": pathspec,
                "format": log_format_name(input.format),
            })))
    }
}

fn no_commits(pathspec: Option<&String>, format: LogFormat) -> ToolOutput {
    let summary = match pathspec {
        Some(p) => format!("git log: no commits touching {p}"),
        None => "git log: no commits".to_owned(),
    };
    ToolOutput::text("[no commits]\n")
        .with_summary(summary)
        .with_metadata(json!({
            "count": 0,
            "more": false,
            "path": pathspec,
            "format": log_format_name(format),
        }))
}

fn log_format_name(format: LogFormat) -> &'static str {
    match format {
        LogFormat::Oneline => "oneline",
        LogFormat::Full => "full",
    }
}
