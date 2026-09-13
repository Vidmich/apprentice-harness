//! Workspace snapshots (task M01-06): what the tree looked like when an
//! agent started and when it ended, so a trajectory records its
//! before and after — the basis of outcome measurement (`files_changed`)
//! and of reproducing the pre-task state for replay.
//!
//! A [`Snapshot`] has two halves. The git half (when the root is inside
//! a work tree and `git` runs): `HEAD`, the branch, whether tracked
//! files differ from `HEAD`, a hash of the porcelain status, the
//! untracked files with their sizes, and `git diff HEAD` of the root's
//! subtree as the blob. The index half, always: a fresh file index
//! (every non-ignored file with size and mtime) reduced to a count and
//! a hash, and kept in memory so [`Snapshot::changes_since`] can name
//! what changed between two snapshots without git.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{debug, warn};

use super::Workspace;
use super::git::{GitError, Repo, Status, parse_status};
use crate::trace::{AgentId, NewEvent, SessionId, kinds, sha256_hex};

/// The diff blob keeps this much at most (head and tail around an
/// omission marker past it).
pub const SNAPSHOT_DIFF_MAX: usize = 16 * 1024 * 1024;
/// Path lists in event payloads stop here (`truncated` says so).
pub const SNAPSHOT_LIST_MAX: usize = 1000;
/// The status output is never expected to reach this.
const STATUS_MAX: usize = 64 * 1024 * 1024;
/// Media type of the diff blob.
pub const DIFF_MEDIA_TYPE: &str = "text/x-diff; charset=utf-8";

/// When in the agent's life a snapshot was taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SnapshotPhase {
    Start,
    End,
}

/// The git half of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSnapshot {
    /// `HEAD`; `None` on an unborn branch.
    pub head: Option<String>,
    /// `None` when detached.
    pub branch: Option<String>,
    /// Tracked files differ from `HEAD` (staged or not).
    pub dirty: bool,
    /// SHA-256 of the porcelain status output.
    pub status_hash: String,
    /// Untracked files under the root with their sizes (`-uall`), sorted.
    pub untracked: Vec<(String, u64)>,
    /// `git diff HEAD` of the root's subtree; `None` without a `HEAD`.
    pub diff: Option<Vec<u8>>,
    /// The diff exceeded [`SNAPSHOT_DIFF_MAX`].
    pub diff_truncated: bool,
}

/// What the workspace looked like at one moment.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// `None` when the root is not a repository or git is not available;
    /// `git_error` says which.
    pub git: Option<GitSnapshot>,
    pub git_error: Option<String>,
    /// Files in the index (ignore rules applied).
    pub file_count: usize,
    /// The index stopped at its limit, so the hash covers a prefix.
    pub index_truncated: bool,
    /// SHA-256 of `path\tsize\tmtime_ns\n` per indexed file.
    pub index_hash: String,
    pub took: Duration,
    entries: Vec<IndexEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexEntry {
    path: String,
    size: u64,
    mtime_ns: Option<u128>,
}

/// The paths that differ between two snapshots' indexes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilesChanged {
    /// Present in both, with a different size or mtime.
    pub changed: Vec<String>,
    pub added: Vec<String>,
    pub deleted: Vec<String>,
}

impl Workspace {
    /// Takes a snapshot now: git status and diff (subprocesses) and a
    /// fresh index (blocking pool). Never fails; a missing git or a
    /// non-repository root leaves the git half empty.
    pub async fn snapshot(self: &Arc<Self>) -> Snapshot {
        let started = Instant::now();
        let ws = Arc::clone(self);
        let index = tokio::task::spawn_blocking(move || ws.refresh());
        let git = git_snapshot(self.root()).await;
        let index = match index.await {
            Ok(index) => index,
            Err(e) => {
                warn!(error = %e, "index build panicked; snapshotting an empty index");
                self.refresh()
            }
        };
        let entries: Vec<IndexEntry> = index
            .entries()
            .iter()
            .map(|e| IndexEntry {
                path: e.path.clone(),
                size: e.size,
                mtime_ns: e.mtime_ns,
            })
            .collect();
        let mut listing = String::new();
        for e in &entries {
            use std::fmt::Write as _;
            let _ = writeln!(
                listing,
                "{}\t{}\t{}",
                e.path,
                e.size,
                e.mtime_ns.map_or_else(String::new, |t| t.to_string())
            );
        }
        let (git, git_error) = match git {
            Ok(git) => (Some(git), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let snapshot = Snapshot {
            git,
            git_error,
            file_count: entries.len(),
            index_truncated: index.is_truncated(),
            index_hash: sha256_hex(listing.as_bytes()),
            took: started.elapsed(),
            entries,
        };
        debug!(
            root = %self.root().display(),
            files = snapshot.file_count,
            git = snapshot.git.is_some(),
            dirty = snapshot.git.as_ref().is_some_and(|g| g.dirty),
            took = ?snapshot.took,
            "workspace snapshot"
        );
        snapshot
    }
}

async fn git_snapshot(root: &std::path::Path) -> Result<GitSnapshot, GitError> {
    let repo = Repo::open(root).await?;
    let out = repo
        .run(
            &[
                "status",
                "--porcelain=v2",
                "-z",
                "--branch",
                "--untracked-files=all",
            ],
            STATUS_MAX,
        )
        .await?;
    let status_hash = sha256_hex(&out.stdout);
    let mut status: Status = parse_status(&out.stdout);
    status.restrict(repo.prefix());
    let mut untracked: Vec<(String, u64)> = status
        .untracked()
        .map(|e| {
            let size = std::fs::metadata(root.join(&e.path)).map_or(0, |m| m.len());
            (e.path.clone(), size)
        })
        .collect();
    untracked.sort_unstable();
    let (diff, diff_truncated) = if status.head.is_some() {
        let out = repo
            .run(
                &[
                    "diff",
                    "HEAD",
                    "--no-color",
                    "--no-ext-diff",
                    "--relative",
                    "--",
                    ".",
                ],
                SNAPSHOT_DIFF_MAX,
            )
            .await?;
        (Some(out.stdout), out.truncated)
    } else {
        (None, false)
    };
    let dirty = status.is_dirty();
    Ok(GitSnapshot {
        head: status.head,
        branch: status.branch,
        dirty,
        status_hash,
        untracked,
        diff,
        diff_truncated,
    })
}

impl Snapshot {
    /// The `workspace.snapshot` event: the payload documented in the
    /// task (`git_head`, `branch`, `dirty`, `status_hash`, `untracked`,
    /// `file_count`, `index_hash`, ...) with the diff as the blob.
    pub fn event(
        &self,
        session: SessionId,
        agent: Option<AgentId>,
        phase: SnapshotPhase,
    ) -> NewEvent {
        let mut payload = json!({
            "phase": phase,
            "git_head": self.git.as_ref().and_then(|g| g.head.clone()),
            "branch": self.git.as_ref().and_then(|g| g.branch.clone()),
            "dirty": self.git.as_ref().map(|g| g.dirty),
            "status_hash": self.git.as_ref().map(|g| g.status_hash.clone()),
            "diff_bytes": self.git.as_ref().and_then(|g| g.diff.as_ref()).map(Vec::len),
            "diff_truncated": self.git.as_ref().map(|g| g.diff_truncated),
            "untracked_count": self.git.as_ref().map(|g| g.untracked.len()),
            "untracked": self.git.as_ref().map(|g| {
                g.untracked
                    .iter()
                    .take(SNAPSHOT_LIST_MAX)
                    .map(|(path, bytes)| json!({"path": path, "bytes": bytes}))
                    .collect::<Vec<_>>()
            }),
            "file_count": self.file_count,
            "index_hash": self.index_hash,
            "index_truncated": self.index_truncated,
            "took_ms": u64::try_from(self.took.as_millis()).unwrap_or(u64::MAX),
        });
        if let Some(e) = &self.git_error {
            payload["git_error"] = json!(e);
        }
        let mut ev = NewEvent::new(session, kinds::WORKSPACE_SNAPSHOT).payload(payload);
        if let Some(agent) = agent {
            ev = ev.agent(agent);
        }
        if let Some(diff) = self.git.as_ref().and_then(|g| g.diff.as_ref())
            && !diff.is_empty()
        {
            ev = ev.blob_bytes(diff.clone(), DIFF_MEDIA_TYPE);
        }
        ev
    }

    /// The indexed files that differ from `before` (an earlier snapshot
    /// of the same root): same path with another size or mtime is
    /// `changed`; the rest is `added` or `deleted`. Ignored files are
    /// invisible to this, as they are to the index.
    pub fn changes_since(&self, before: &Self) -> FilesChanged {
        let mut out = FilesChanged::default();
        let (mut a, mut b) = (
            before.entries.iter().peekable(),
            self.entries.iter().peekable(),
        );
        loop {
            match (a.peek(), b.peek()) {
                (None, None) => break,
                (Some(x), None) => {
                    out.deleted.push(x.path.clone());
                    a.next();
                }
                (None, Some(y)) => {
                    out.added.push(y.path.clone());
                    b.next();
                }
                (Some(x), Some(y)) => match x.path.cmp(&y.path) {
                    std::cmp::Ordering::Less => {
                        out.deleted.push(x.path.clone());
                        a.next();
                    }
                    std::cmp::Ordering::Greater => {
                        out.added.push(y.path.clone());
                        b.next();
                    }
                    std::cmp::Ordering::Equal => {
                        if x.size != y.size || x.mtime_ns != y.mtime_ns {
                            out.changed.push(y.path.clone());
                        }
                        a.next();
                        b.next();
                    }
                },
            }
        }
        out
    }
}

impl FilesChanged {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.added.is_empty() && self.deleted.is_empty()
    }

    pub fn len(&self) -> usize {
        self.changed.len() + self.added.len() + self.deleted.len()
    }

    /// The `outcome {kind: "files_changed"}` event. Lists are cut at
    /// [`SNAPSHOT_LIST_MAX`] each; the counts are always exact.
    pub fn event(&self, session: SessionId, agent: Option<AgentId>) -> NewEvent {
        let cut = |v: &[String]| {
            v.iter()
                .take(SNAPSHOT_LIST_MAX)
                .cloned()
                .collect::<Vec<_>>()
        };
        let truncated = [&self.changed, &self.added, &self.deleted]
            .iter()
            .any(|v| v.len() > SNAPSHOT_LIST_MAX);
        let mut details = json!({
            "changed": cut(&self.changed),
            "added": cut(&self.added),
            "deleted": cut(&self.deleted),
            "counts": {
                "changed": self.changed.len(),
                "added": self.added.len(),
                "deleted": self.deleted.len(),
            },
        });
        if truncated {
            details["truncated"] = json!(true);
        }
        let mut ev = NewEvent::new(session, kinds::OUTCOME).payload(json!({
            "kind": "files_changed",
            "details": details,
        }));
        if let Some(agent) = agent {
            ev = ev.agent(agent);
        }
        ev
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(entries: &[(&str, u64, u128)]) -> Snapshot {
        Snapshot {
            git: None,
            git_error: None,
            file_count: entries.len(),
            index_truncated: false,
            index_hash: String::new(),
            took: Duration::ZERO,
            entries: entries
                .iter()
                .map(|(p, s, t)| IndexEntry {
                    path: (*p).to_owned(),
                    size: *s,
                    mtime_ns: Some(*t),
                })
                .collect(),
        }
    }

    #[test]
    fn changes_are_a_merge_of_two_sorted_listings() {
        let before = snap(&[
            ("a.rs", 1, 1),
            ("b.rs", 2, 2),
            ("c.rs", 3, 3),
            ("d.rs", 4, 4),
        ]);
        let after = snap(&[
            ("a.rs", 1, 1),
            ("b.rs", 2, 9),
            ("c.rs", 5, 3),
            ("e.rs", 1, 1),
        ]);
        let changes = after.changes_since(&before);
        assert_eq!(changes.changed, ["b.rs", "c.rs"]);
        assert_eq!(changes.added, ["e.rs"]);
        assert_eq!(changes.deleted, ["d.rs"]);
        assert_eq!(changes.len(), 4);
        assert!(after.changes_since(&after).is_empty());
        let ev = changes.event(SessionId::generate(), None);
        assert_eq!(ev.kind, kinds::OUTCOME);
        assert_eq!(ev.payload["kind"], "files_changed");
        assert_eq!(ev.payload["details"]["deleted"], json!(["d.rs"]));
        assert_eq!(ev.payload["details"]["counts"]["changed"], 2);
        assert_eq!(ev.payload["details"].get("truncated"), None);
    }

    #[test]
    fn outcome_lists_are_cut_with_exact_counts() {
        let big = FilesChanged {
            changed: (0..SNAPSHOT_LIST_MAX + 5)
                .map(|i| format!("f{i}"))
                .collect(),
            ..FilesChanged::default()
        };
        let ev = big.event(SessionId::generate(), Some(AgentId::generate()));
        let details = &ev.payload["details"];
        assert_eq!(
            details["changed"].as_array().unwrap().len(),
            SNAPSHOT_LIST_MAX
        );
        assert_eq!(details["counts"]["changed"], SNAPSHOT_LIST_MAX + 5);
        assert_eq!(details["truncated"], true);
        assert!(ev.agent.is_some());
    }
}
