//! Running `git` as a subprocess. No git library: the binary on `PATH`
//! is what the user has, and a missing one is a clear, reportable
//! condition rather than a build dependency.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt as _;
use tokio::process::Command;
use tracing::debug;

use crate::tools::shell::capture::Capture;

/// A git command that runs longer than this is killed.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(30);
/// Read size per pipe.
const CHUNK: usize = 64 * 1024;
/// Stderr kept for error messages.
const STDERR_MAX: usize = 16 * 1024;

/// Why a git command produced no output.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// No `git` on `PATH`.
    #[error("git is not available (no `git` on PATH)")]
    NotAvailable,
    /// The directory is not inside a git work tree.
    #[error("`{root}` is not a git repository")]
    NotARepo { root: String },
    #[error("`git {args}` did not finish within {}s", GIT_TIMEOUT.as_secs())]
    TimedOut { args: String },
    /// Non-zero exit; `stderr` is git's message, trimmed.
    #[error("`git {args}` failed{}: {stderr}", code.map(|c| format!(" (exit {c})")).unwrap_or_default())]
    Failed {
        args: String,
        code: Option<i32>,
        stderr: String,
    },
    #[error("cannot run git: {0}")]
    Io(#[from] std::io::Error),
}

impl GitError {
    /// Stable kind for metadata and logs.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::NotAvailable => "not_available",
            Self::NotARepo { .. } => "not_a_repo",
            Self::TimedOut { .. } => "timed_out",
            Self::Failed { .. } => "failed",
            Self::Io(_) => "io",
        }
    }

    /// Git's own message for a failed command, when there is one.
    pub fn stderr(&self) -> Option<&str> {
        match self {
            Self::Failed { stderr, .. } => Some(stderr),
            _ => None,
        }
    }
}

/// What a successful git command printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    /// Stdout, head and tail around a `[... N bytes omitted]` line when
    /// it exceeded the cap.
    pub stdout: Vec<u8>,
    /// Stderr (warnings), lossily decoded and trimmed.
    pub stderr: String,
    /// Stdout exceeded the cap.
    pub truncated: bool,
}

impl GitOutput {
    /// Stdout as text (lossy).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// A directory inside a git work tree, ready to run commands from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    root: PathBuf,
    prefix: String,
}

impl Repo {
    /// Checks that `root` is inside a work tree and finds its path
    /// relative to the top (`""` at the top, `sub/dir/` below it), so
    /// output that git reports relative to the top can be made
    /// relative to `root`.
    ///
    /// # Errors
    /// [`GitError::NotAvailable`], [`GitError::NotARepo`], or a failure
    /// of `rev-parse`.
    pub async fn open(root: &Path) -> Result<Self, GitError> {
        let out = run(root, &["rev-parse", "--show-prefix"], 4096).await?;
        let prefix = out.text().trim().replace('\\', "/");
        Ok(Self {
            root: root.to_path_buf(),
            prefix,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The root's path under the top of the work tree, with a trailing
    /// `/` unless it is the top itself.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// `path` as git reports it (relative to the top of the work tree)
    /// made relative to the root; `None` when it lies outside the root.
    pub fn relative(&self, top_relative: &str) -> Option<String> {
        top_relative
            .strip_prefix(&self.prefix)
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
    }

    /// Runs `git -C <root> <args>` keeping up to `max_stdout` bytes of
    /// stdout.
    ///
    /// # Errors
    /// A non-zero exit is [`GitError::Failed`]; see [`run`].
    pub async fn run(&self, args: &[&str], max_stdout: usize) -> Result<GitOutput, GitError> {
        run(&self.root, args, max_stdout).await
    }
}

/// Runs `git -C <cwd> <args>` with the harness environment, under
/// [`GIT_TIMEOUT`], keeping up to `max_stdout` bytes of stdout (head
/// and tail around an omission marker past that).
///
/// # Errors
/// [`GitError::NotAvailable`] when the binary is missing;
/// [`GitError::NotARepo`] when git says so; [`GitError::Failed`] for
/// any other non-zero exit; [`GitError::TimedOut`].
pub async fn run(cwd: &Path, args: &[&str], max_stdout: usize) -> Result<GitOutput, GitError> {
    let shown = args.join(" ");
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(cwd)
        .arg("--no-pager")
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(GitError::NotAvailable),
        Err(e) => return Err(e.into()),
    };
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let read = async {
        let mut out = Capture::new(max_stdout);
        let mut err = Vec::new();
        let read_out = async {
            let mut buf = vec![0u8; CHUNK];
            while let Ok(n) = stdout.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                out.push(&buf[..n]);
            }
        };
        let read_err = async {
            let mut buf = vec![0u8; CHUNK];
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                if err.len() < STDERR_MAX {
                    err.extend_from_slice(&buf[..n.min(STDERR_MAX - err.len())]);
                }
            }
        };
        tokio::join!(read_out, read_err);
        let status = child.wait().await;
        (out, err, status)
    };
    let Ok((out, err, status)) = tokio::time::timeout(GIT_TIMEOUT, read).await else {
        debug!(args = %shown, "git timed out");
        let _ = child.kill().await;
        return Err(GitError::TimedOut { args: shown });
    };
    let status = status?;
    let stderr = String::from_utf8_lossy(&err).trim().to_owned();
    if !status.success() {
        if stderr.contains("not a git repository") {
            return Err(GitError::NotARepo {
                root: cwd.to_string_lossy().into_owned(),
            });
        }
        return Err(GitError::Failed {
            args: shown,
            code: status.code(),
            stderr,
        });
    }
    let truncated = out.truncated();
    let stdout = out.render(|n| format!("\n[... {n} bytes omitted]\n"));
    Ok(GitOutput {
        stdout,
        stderr,
        truncated,
    })
}
