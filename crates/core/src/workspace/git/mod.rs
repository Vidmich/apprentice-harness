//! Git for the workspace (tasks M01-02, M01-06).
//!
//! [`git_head`] reads `HEAD` from `.git` directly (no git binary, no
//! libgit2): enough for `workspace.info`. Everything else shells out
//! through [`Repo`] (`git -C <root> ...` with `GIT_OPTIONAL_LOCKS=0`,
//! `LC_ALL=C`, a 30 s timeout): the porcelain [`Status`] parser serves
//! the `git_status` tool and the workspace snapshot alike.

mod run;
mod status;

use std::path::{Path, PathBuf};

pub use run::{GIT_TIMEOUT, GitError, GitOutput, Repo};
pub use status::{Entry, Status, parse_status};

/// Where the repository's `HEAD` points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHead {
    /// Full commit hash; `None` on an unborn branch (fresh `git init`).
    pub commit: Option<String>,
    /// Branch name when `HEAD` is symbolic (`refs/heads/<name>`).
    pub branch: Option<String>,
}

/// Reads `HEAD` for the repository whose work tree is `root`. `None`
/// when `root` is not the top of a work tree (a `.git` directory or a
/// `gitdir:` file of a worktree/submodule).
pub fn git_head(root: &Path) -> Option<GitHead> {
    let git_dir = git_dir(root)?;
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        let branch = reference.strip_prefix("refs/heads/").map(str::to_owned);
        let commit = resolve_ref(&git_dir, reference);
        return Some(GitHead { commit, branch });
    }
    (!head.is_empty() && head.chars().all(|c| c.is_ascii_hexdigit())).then(|| GitHead {
        commit: Some(head.to_owned()),
        branch: None,
    })
}

/// `<root>/.git` as a directory, or the directory a `.git` file names.
fn git_dir(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let target = text.trim().strip_prefix("gitdir:")?.trim();
    let target = Path::new(target);
    let dir = if target.is_absolute() {
        target.to_path_buf()
    } else {
        root.join(target)
    };
    dir.is_dir().then_some(dir)
}

/// Loose ref file first, then `packed-refs`. For a linked worktree the
/// loose refs live in the common dir (`commondir` file).
fn resolve_ref(git_dir: &Path, reference: &str) -> Option<String> {
    let common = std::fs::read_to_string(git_dir.join("commondir")).map_or_else(
        |_| git_dir.to_path_buf(),
        |s| {
            let p = Path::new(s.trim());
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                git_dir.join(p)
            }
        },
    );
    if let Ok(text) = std::fs::read_to_string(common.join(reference)) {
        let hash = text.trim();
        if !hash.is_empty() {
            return Some(hash.to_owned());
        }
    }
    let packed = std::fs::read_to_string(common.join("packed-refs")).ok()?;
    packed
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with('^'))
        .find_map(|l| {
            let (hash, name) = l.split_once(' ')?;
            (name.trim() == reference).then(|| hash.trim().to_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn no_repo_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(git_head(dir.path()), None);
    }

    #[test]
    fn symbolic_head_with_loose_ref() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join(".git");
        std::fs::create_dir_all(git.join("refs/heads")).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(
            git_head(dir.path()),
            Some(GitHead {
                commit: None,
                branch: Some("main".into())
            })
        );
        std::fs::write(git.join("refs/heads/main"), format!("{SHA}\n")).unwrap();
        assert_eq!(
            git_head(dir.path()),
            Some(GitHead {
                commit: Some(SHA.into()),
                branch: Some("main".into())
            })
        );
    }

    #[test]
    fn packed_ref_and_detached_head() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join(".git");
        std::fs::create_dir_all(&git).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/feature/x\n").unwrap();
        std::fs::write(
            git.join("packed-refs"),
            format!("# pack-refs with: peeled fully-peeled sorted\n{SHA} refs/heads/feature/x\n^deadbeef\n"),
        )
        .unwrap();
        let head = git_head(dir.path()).unwrap();
        assert_eq!(head.commit.as_deref(), Some(SHA));
        assert_eq!(head.branch.as_deref(), Some("feature/x"));

        std::fs::write(git.join("HEAD"), format!("{SHA}\n")).unwrap();
        let head = git_head(dir.path()).unwrap();
        assert_eq!(head.commit.as_deref(), Some(SHA));
        assert_eq!(head.branch, None);
    }

    #[test]
    fn gitdir_file_is_followed() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("elsewhere");
        std::fs::create_dir_all(real.join("refs/heads")).unwrap();
        std::fs::write(real.join("HEAD"), "ref: refs/heads/dev\n").unwrap();
        std::fs::write(real.join("refs/heads/dev"), SHA).unwrap();
        let root = dir.path().join("tree");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".git"), "gitdir: ../elsewhere\n").unwrap();
        let head = git_head(&root).unwrap();
        assert_eq!(head.branch.as_deref(), Some("dev"));
        assert_eq!(head.commit.as_deref(), Some(SHA));
    }
}
