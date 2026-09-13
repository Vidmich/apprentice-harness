//! The mentor's system prompt (task M01-09): a frozen core embedded
//! from `prompts/mentor_system_v1.md` — the same bytes for everyone,
//! so its cache entry is shared across sessions — and a `#workspace`
//! block built once at the start of a session (host, root, git state,
//! languages, top-level entries, the project's `.harness/HARNESS.md`).
//! Nothing volatile goes in: the time, the turn, the agent. Each block
//! gets its own cache breakpoint when the conversation builds a request
//! (see [`super::conversation`]).
//!
//! The version string travels with every session (`config_json`) and
//! every `mentor.request` payload (`prompt_version`); a wording change
//! bumps it and gets a line in `prompts/CHANGELOG.md`.

use std::path::Path;
use std::sync::Arc;

use crate::config::Config;
use crate::mentor::SystemBlock;
use crate::workspace::git::{GitError, Repo, parse_status};
use crate::workspace::{FileIndex, Workspace};

/// The version recorded with sessions and requests.
pub const PROMPT_VERSION: &str = "mentor_system_v1";

/// The frozen core, verbatim.
pub const MENTOR_SYSTEM_V1: &str = include_str!("../../prompts/mentor_system_v1.md");

/// `.harness/HARNESS.md` past this many bytes is cut, with a note.
pub const INSTRUCTIONS_MAX: usize = 8 * 1024;
/// Top-level entries listed at most.
pub const TOP_LEVEL_MAX: usize = 40;
/// Languages listed at most.
pub const LANGUAGES_MAX: usize = 6;

/// The assembled system prompt of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPrompt {
    pub version: String,
    /// `[core, workspace context]`.
    pub blocks: Vec<SystemBlock>,
}

/// Facts about the machine the daemon runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// `windows 11 (26100)`, `linux 6.8`, ...
    pub os: String,
    /// `pwsh`, `bash`, ...
    pub shell: String,
    /// The harness version.
    pub harness: String,
}

impl Host {
    /// The running machine, with the shell `config` selects.
    pub fn detect(config: &Config) -> Self {
        let name = sysinfo::System::name().unwrap_or_else(|| std::env::consts::OS.to_owned());
        let os = match sysinfo::System::os_version() {
            Some(v) if !v.is_empty() => format!("{} {v}", name.to_lowercase()),
            _ => name.to_lowercase(),
        };
        Self {
            os,
            shell: crate::tools::shell::shell_name(&config.tools.shell),
            harness: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

/// The project's `.harness/HARNESS.md` as it goes into the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instructions {
    /// At most [`INSTRUCTIONS_MAX`] bytes of the file.
    pub text: String,
    /// The file's full size when `text` is a prefix of it.
    pub truncated_from: Option<usize>,
}

/// What the `#workspace` block says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceContext {
    pub host: Host,
    /// The root, `/`-separated; `None` for a session without a
    /// workspace.
    pub root: Option<String>,
    /// One line: `branch main @ 3f2a1c9, 2 modified, 1 untracked`, or
    /// why there is none.
    pub git: Option<String>,
    /// `(language, percent of indexed files)`, largest first.
    pub languages: Vec<(String, u8)>,
    /// Root entries in walk order, directories with a trailing `/`.
    pub top_level: Vec<String>,
    /// Root entries past [`TOP_LEVEL_MAX`].
    pub top_level_more: usize,
    pub instructions: Option<Instructions>,
}

impl WorkspaceContext {
    /// Everything about `workspace` the block needs: `git status`
    /// (a subprocess), the file index (built on the blocking pool when
    /// stale) and the instructions file.
    pub async fn gather(workspace: Option<&Arc<Workspace>>, host: Host) -> Self {
        let Some(ws) = workspace else {
            return Self::without_workspace(host);
        };
        let git = git_line(ws.root()).await;
        let for_index = Arc::clone(ws);
        let index = tokio::task::spawn_blocking(move || for_index.index())
            .await
            .unwrap_or_else(|_| ws.index());
        let (top_level, top_level_more) = top_level(ws);
        let instructions = match ws.instructions() {
            Ok(text) => text.map(|t| instructions(&t)),
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the project instructions");
                None
            }
        };
        Self {
            host,
            root: Some(ws.root_string().replace('\\', "/")),
            git: Some(git),
            languages: languages(&index),
            top_level,
            top_level_more,
            instructions,
        }
    }

    /// The block of a session that has no workspace.
    pub fn without_workspace(host: Host) -> Self {
        Self {
            host,
            root: None,
            git: None,
            languages: Vec::new(),
            top_level: Vec::new(),
            top_level_more: 0,
            instructions: None,
        }
    }

    /// The block's text.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::from("#workspace\n");
        match &self.root {
            Some(root) => {
                let _ = writeln!(s, "root: {root}");
            }
            None => s.push_str("root: none (no workspace is open; file, search, git and shell tools have nothing to work on)\n"),
        }
        let _ = writeln!(
            s,
            "os: {} · shell: {} · harness {}",
            self.host.os, self.host.shell, self.host.harness
        );
        if let Some(git) = &self.git {
            let _ = writeln!(s, "git: {git}");
        }
        if !self.languages.is_empty() {
            let list: Vec<String> = self
                .languages
                .iter()
                .map(|(l, p)| format!("{l} {p}%"))
                .collect();
            let _ = writeln!(s, "languages: {}", list.join(", "));
        }
        if !self.top_level.is_empty() {
            let mut line = self.top_level.join(", ");
            if self.top_level_more > 0 {
                let _ = write!(line, " … (+{} more)", self.top_level_more);
            }
            let _ = writeln!(s, "top-level: {line}");
        }
        if let Some(i) = &self.instructions {
            s.push_str("#instructions (from .harness/HARNESS.md)\n");
            s.push_str(&i.text);
            if !i.text.ends_with('\n') {
                s.push('\n');
            }
            if let Some(full) = i.truncated_from {
                let _ = writeln!(
                    s,
                    "[truncated: the first {} of {full} bytes are shown; keep the file under {} bytes]",
                    i.text.len(),
                    INSTRUCTIONS_MAX
                );
            }
        }
        s
    }
}

/// The system prompt of a new session on `workspace` under `config`:
/// the core, then the workspace block.
pub async fn build_system(workspace: Option<&Arc<Workspace>>, config: &Config) -> SystemPrompt {
    let context = WorkspaceContext::gather(workspace, Host::detect(config)).await;
    assemble(&context)
}

/// The two blocks for an already gathered context.
pub fn assemble(context: &WorkspaceContext) -> SystemPrompt {
    SystemPrompt {
        version: PROMPT_VERSION.to_owned(),
        blocks: vec![
            SystemBlock::new(MENTOR_SYSTEM_V1),
            SystemBlock::new(context.render()),
        ],
    }
}

/// `branch main @ 3f2a1c9, 2 modified, 1 untracked`; `clean` when
/// nothing is; `detached @ ...`; `branch x (no commits yet)`; or the
/// reason git has nothing to say.
async fn git_line(root: &Path) -> String {
    use std::fmt::Write as _;
    let repo = match Repo::open(root).await {
        Ok(repo) => repo,
        Err(GitError::NotARepo { .. }) => return "not a git repository".to_owned(),
        Err(GitError::NotAvailable) => return "git is not installed".to_owned(),
        Err(e) => return format!("unavailable ({e})"),
    };
    let out = match repo
        .run(
            &[
                "status",
                "--porcelain=v2",
                "-z",
                "--branch",
                "--untracked-files=all",
            ],
            16 * 1024 * 1024,
        )
        .await
    {
        Ok(out) => out,
        Err(e) => return format!("unavailable ({e})"),
    };
    let mut status = parse_status(&out.stdout);
    status.restrict(repo.prefix());
    let mut line = match (&status.branch, &status.head) {
        (Some(branch), Some(head)) => format!("branch {branch} @ {}", short(head)),
        (Some(branch), None) => format!("branch {branch} (no commits yet)"),
        (None, Some(head)) => format!("detached @ {}", short(head)),
        (None, None) => "no HEAD".to_owned(),
    };
    let modified = status.entries.iter().filter(|e| !e.untracked).count();
    let untracked = status.untracked().count();
    if modified == 0 && untracked == 0 {
        line.push_str(", clean");
    } else {
        if modified > 0 {
            let _ = write!(line, ", {modified} modified");
        }
        if untracked > 0 {
            let _ = write!(line, ", {untracked} untracked");
        }
    }
    line
}

fn short(hash: &str) -> &str {
    hash.get(..7).unwrap_or(hash)
}

/// Languages by share of indexed files, the largest [`LANGUAGES_MAX`]
/// of those at 1% or more. Files with no known language are counted
/// in the total, so the shares need not add up to 100.
fn languages(index: &FileIndex) -> Vec<(String, u8)> {
    let total = index.len();
    if total == 0 {
        return Vec::new();
    }
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for e in index.entries() {
        if let Some(l) = e.language {
            *counts.entry(l).or_default() += 1;
        }
    }
    let mut list: Vec<(&str, usize)> = counts.into_iter().collect();
    list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    list.into_iter()
        .map(|(l, n)| (l.to_owned(), u8::try_from(n * 100 / total).unwrap_or(100)))
        .filter(|(_, p)| *p >= 1)
        .take(LANGUAGES_MAX)
        .collect()
}

/// The root's entries (ignore rules applied), directories first with
/// a trailing `/`, then files, each sorted; and how many were left
/// out past [`TOP_LEVEL_MAX`].
fn top_level(ws: &Workspace) -> (Vec<String>, usize) {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in ws.walker_at(ws.root(), Some(1)).flatten() {
        if entry.depth() != 1 {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            dirs.push(format!("{name}/"));
        } else {
            files.push(name);
        }
    }
    dirs.sort_unstable();
    files.sort_unstable();
    let mut all = dirs;
    all.extend(files);
    let more = all.len().saturating_sub(TOP_LEVEL_MAX);
    all.truncate(TOP_LEVEL_MAX);
    (all, more)
}

/// The instructions, cut at a character boundary past
/// [`INSTRUCTIONS_MAX`] bytes.
fn instructions(text: &str) -> Instructions {
    if text.len() <= INSTRUCTIONS_MAX {
        return Instructions {
            text: text.to_owned(),
            truncated_from: None,
        };
    }
    let mut cut = INSTRUCTIONS_MAX;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    Instructions {
        text: text[..cut].to_owned(),
        truncated_from: Some(text.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Host {
        Host {
            os: "windows 11 (26100)".into(),
            shell: "pwsh".into(),
            harness: "0.1.0".into(),
        }
    }

    #[test]
    fn the_core_is_short_frozen_and_first() {
        // Roughly 4 characters per token: under the 1200-token budget.
        assert!(MENTOR_SYSTEM_V1.len() < 4800, "{}", MENTOR_SYSTEM_V1.len());
        assert!(MENTOR_SYSTEM_V1.starts_with("You are the mentor model of apprentice-harness"));
        assert!(!MENTOR_SYSTEM_V1.contains('{'), "no interpolation markers");
        assert!(
            !MENTOR_SYSTEM_V1.contains("apprentice model"),
            "baseline: no apprentice text"
        );
        let prompt = assemble(&WorkspaceContext::without_workspace(host()));
        assert_eq!(prompt.version, PROMPT_VERSION);
        assert_eq!(prompt.blocks.len(), 2);
        assert_eq!(prompt.blocks[0], SystemBlock::new(MENTOR_SYSTEM_V1));
        assert!(prompt.blocks[1].text.starts_with("#workspace\nroot: none"));
        assert!(
            prompt.blocks[1]
                .text
                .contains("os: windows 11 (26100) · shell: pwsh · harness 0.1.0")
        );
    }

    #[test]
    fn instructions_are_cut_at_a_char_boundary() {
        let short = instructions("keep it");
        assert_eq!(short.truncated_from, None);
        let mut long = "é".repeat(INSTRUCTIONS_MAX / 2 - 1); // 2 bytes each
        long.push_str("é€"); // crosses the limit inside a character
        assert!(long.len() > INSTRUCTIONS_MAX);
        let cut = instructions(&long);
        assert_eq!(cut.truncated_from, Some(long.len()));
        assert!(cut.text.len() <= INSTRUCTIONS_MAX);
        assert!(cut.text.chars().all(|c| c == 'é'));
        let rendered = WorkspaceContext {
            instructions: Some(cut),
            ..WorkspaceContext::without_workspace(host())
        }
        .render();
        assert!(rendered.contains("#instructions (from .harness/HARNESS.md)\n"));
        assert!(rendered.contains(&format!(
            "[truncated: the first {} of {} bytes are shown",
            INSTRUCTIONS_MAX,
            long.len()
        )));
    }

    #[test]
    fn render_lists_only_what_is_there() {
        let mut ctx = WorkspaceContext::without_workspace(host());
        ctx.root = Some("C:/src/foo".into());
        ctx.git = Some("branch main @ 3f2a1c9, 2 modified, 1 untracked".into());
        ctx.languages = vec![("rust".into(), 61), ("typescript".into(), 30)];
        ctx.top_level = vec!["crates/".into(), "Cargo.toml".into()];
        ctx.top_level_more = 3;
        assert_eq!(
            ctx.render(),
            "#workspace\n\
             root: C:/src/foo\n\
             os: windows 11 (26100) · shell: pwsh · harness 0.1.0\n\
             git: branch main @ 3f2a1c9, 2 modified, 1 untracked\n\
             languages: rust 61%, typescript 30%\n\
             top-level: crates/, Cargo.toml … (+3 more)\n"
        );
    }
}
