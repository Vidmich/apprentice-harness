//! Goldens for the three tools against a fixture repository built in
//! the test (init, commit, modify, stage, rename, add untracked), the
//! per-file diff cut, the non-repository case, and the workspace
//! snapshots around an edit. Every test needs `git` on `PATH` and says
//! so when it is missing.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::tools::{SeenFiles, ToolContent, ToolEnv, ToolValidator};
use crate::trace::{AgentId, SessionId, kinds};
use crate::workspace::{SnapshotPhase, Workspace};

/// Whether `git` runs here; prints the skip reason once when not.
fn have_git() -> bool {
    let ok = std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("skipping: git is not on PATH");
    }
    ok
}

macro_rules! require_git {
    () => {
        if !have_git() {
            return;
        }
    };
}

/// Runs git in `dir` with fixed dates; panics on failure.
fn git(dir: &Path, args: &[&str]) -> String {
    git_at(dir, args, "2024-01-02T10:00:00+0000")
}

fn git_at(dir: &Path, args: &[&str], date: &str) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

struct Fixture {
    dir: tempfile::TempDir,
    ws: Arc<Workspace>,
}

impl Fixture {
    /// An empty directory, not a repository.
    fn plain() -> Self {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "README.md", "# plain\n");
        write(dir.path(), "src/a.rs", "fn a() {}\n");
        let ws = Arc::new(Workspace::open(dir.path()).unwrap());
        Self { dir, ws }
    }

    /// A repository with one commit of four files, clean.
    fn repo() -> Self {
        let f = Self::plain();
        let root = f.dir.path();
        git(root, &["init", "-q"]);
        git(root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git(root, &["config", "user.name", "Test User"]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["config", "core.autocrlf", "false"]);
        git(root, &["config", "commit.gpgsign", "false"]);
        write(root, "src/b.rs", "fn b() {}\n");
        write(root, "old.txt", "old\n");
        git(root, &["add", "."]);
        git(root, &["commit", "-q", "-m", "Initial commit"]);
        f
    }

    /// The repository with staged, unstaged, renamed and untracked
    /// paths: `src/a.rs` staged and modified again, `README.md`
    /// modified, `old.txt` renamed to `renamed.txt`, `new.txt` new.
    fn dirty() -> Self {
        let f = Self::repo();
        let root = f.dir.path();
        write(root, "src/a.rs", "fn a() {}\nfn a2() {}\n");
        git(root, &["add", "src/a.rs"]);
        write(root, "src/a.rs", "fn a() {}\nfn a2() {}\nfn a3() {}\n");
        write(root, "README.md", "# plain\n\nmore\n");
        git(root, &["mv", "old.txt", "renamed.txt"]);
        write(root, "new.txt", "new\n");
        f
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn ctx(&self) -> ToolContext {
        let (progress, _rx) = mpsc::channel(1);
        ToolContext {
            workspace: Some(Arc::clone(&self.ws)),
            session_id: SessionId::generate(),
            agent_id: AgentId::generate(),
            call_id: "t1".into(),
            env: ToolEnv::default(),
            config: Arc::default(),
            seen: Arc::new(SeenFiles::new()),
            progress,
        }
    }

    async fn call(&self, name: &str, input: Value) -> Result<ToolOutput, ToolError> {
        let tool = git_tools()
            .into_iter()
            .find(|t| t.spec().name == name)
            .unwrap();
        ToolValidator::compile(&tool.spec().input_schema)
            .unwrap()
            .validate(&input)?;
        tool.call(&self.ctx(), input, CancellationToken::new())
            .await
    }

    async fn run(&self, name: &str, input: Value) -> ToolOutput {
        self.call(name, input).await.unwrap()
    }
}

fn text(out: &ToolOutput) -> String {
    match &out.content {
        ToolContent::Text(t) => t.replace("\r\n", "\n"),
        other => panic!("not text: {other:?}"),
    }
}

#[tokio::test]
async fn status_groups_paths_by_state() {
    require_git!();
    let f = Fixture::dirty();
    let out = f.run("git_status", json!({})).await;
    assert_eq!(
        text(&out),
        "branch main\n\
         staged:\n  \
           R old.txt -> renamed.txt\n  \
           M src/a.rs\n\
         unstaged:\n  \
           M README.md\n  \
           M src/a.rs\n\
         untracked:\n  \
           new.txt\n"
    );
    assert_eq!(out.summary, "git: main, 2 staged, 2 modified, 1 untracked");
    assert!(!out.is_error);
    assert_eq!(out.metadata["branch"], "main");
    assert_eq!(out.metadata["staged"], 2);
    assert_eq!(out.metadata["untracked"], 1);
    assert_eq!(out.metadata["clean"], false);
    assert_eq!(out.metadata["head"].as_str().unwrap().len(), 40);
}

#[tokio::test]
async fn status_of_a_clean_tree_and_of_a_fresh_repo() {
    require_git!();
    let f = Fixture::repo();
    let out = f.run("git_status", json!({})).await;
    assert_eq!(
        text(&out),
        "branch main\nnothing to commit, working tree clean\n"
    );
    assert_eq!(out.summary, "git: main, clean");
    assert_eq!(out.metadata["clean"], true);

    let fresh = Fixture::plain();
    git(fresh.root(), &["init", "-q"]);
    git(fresh.root(), &["symbolic-ref", "HEAD", "refs/heads/main"]);
    let out = fresh.run("git_status", json!({})).await;
    assert_eq!(
        text(&out),
        "branch main (no commits yet)\nuntracked:\n  README.md\n  src/\n"
    );
    assert_eq!(out.summary, "git: main, 2 untracked");
    let log = fresh.run("git_log", json!({})).await;
    assert_eq!(text(&log), "[no commits]\n");
    assert_eq!(log.summary, "git log: no commits");
    assert!(!log.is_error);
}

#[tokio::test]
async fn diff_shows_the_working_tree_the_index_or_a_base() {
    require_git!();
    let f = Fixture::dirty();

    let out = f.run("git_diff", json!({})).await;
    let t = text(&out);
    assert!(t.contains("+++ b/README.md"), "{t}");
    assert!(t.contains("+++ b/src/a.rs"), "{t}");
    assert!(t.contains("+fn a3() {}"), "{t}");
    assert!(!t.contains("+fn a2() {}"), "{t}");
    assert_eq!(out.summary, "git diff: 2 files, +3 \u{2212}0");
    assert_eq!(out.metadata["files"], 2);
    assert_eq!(out.metadata["staged"], false);
    assert_eq!(out.metadata["path"], Value::Null);

    let out = f.run("git_diff", json!({"staged": true})).await;
    let t = text(&out);
    assert!(t.contains("rename from old.txt"), "{t}");
    assert!(t.contains("+fn a2() {}"), "{t}");
    assert!(!t.contains("README.md"), "{t}");
    assert_eq!(out.summary, "git diff: 2 files, +1 \u{2212}0");

    let out = f
        .run("git_diff", json!({"base": "HEAD", "path": "src"}))
        .await;
    let t = text(&out);
    assert!(
        t.contains("+fn a2() {}") && t.contains("+fn a3() {}"),
        "{t}"
    );
    assert!(!t.contains("README.md"), "{t}");
    assert_eq!(out.summary, "git diff: 1 file, +2 \u{2212}0");
    assert_eq!(out.metadata["path"], "src");
    assert_eq!(out.metadata["base"], "HEAD");

    let out = f.run("git_diff", json!({"path": "src/b.rs"})).await;
    assert_eq!(text(&out), "[no changes]\n");
    assert_eq!(out.summary, "git diff: no unstaged changes");
    assert_eq!(out.metadata["files"], 0);

    // A bad ref is git's error, shown as an error result.
    let out = f.run("git_diff", json!({"base": "nope"})).await;
    assert!(out.is_error);
    assert_eq!(out.summary, "git diff: failed");
    assert!(text(&out).contains("nope"), "{}", text(&out));
    assert_eq!(out.metadata["error"], "failed");

    // Options cannot be smuggled in as refs or paths.
    let err = f.call("git_diff", json!({"base": "--output=x"})).await;
    assert!(matches!(err, Err(ToolError::InvalidInput(_))), "{err:?}");
    let err = f.call("git_diff", json!({"path": "../x"})).await;
    assert!(matches!(err, Err(ToolError::Denied(_))), "{err:?}");
}

/// Every hunk in `diff` has exactly the lines its header announces.
fn hunks_are_whole(diff: &str) {
    let mut lines = diff.lines().peekable();
    let mut hunks = 0;
    while let Some(line) = lines.next() {
        let Some(header) = line.strip_prefix("@@ ") else {
            continue;
        };
        hunks += 1;
        let counts = |spec: &str| -> usize {
            spec[1..]
                .split_once(',')
                .map_or(1, |(_, n)| n.parse().unwrap())
        };
        let mut parts = header.split(' ');
        let old = counts(parts.next().unwrap());
        let new = counts(parts.next().unwrap());
        let (mut seen_old, mut seen_new) = (0, 0);
        while let Some(l) = lines.peek() {
            match l.as_bytes().first() {
                Some(b' ') => {
                    seen_old += 1;
                    seen_new += 1;
                }
                Some(b'-') => seen_old += 1,
                Some(b'+') => seen_new += 1,
                _ => break,
            }
            lines.next();
        }
        assert_eq!((seen_old, seen_new), (old, new), "hunk `{line}` is cut");
    }
    assert!(hunks > 0, "no hunks in {diff}");
}

#[tokio::test]
async fn diff_truncation_never_splits_a_hunk() {
    require_git!();
    let f = Fixture::repo();
    let root = f.root();
    let lines: Vec<String> = (1..=300).map(|i| format!("line {i}")).collect();
    write(root, "big.txt", &(lines.join("\n") + "\n"));
    write(root, "z.txt", "z\n");
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "big"]);
    let edited: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if i % 10 == 0 {
                format!("{l} changed")
            } else {
                l.clone()
            }
        })
        .collect();
    write(root, "big.txt", &(edited.join("\n") + "\n"));
    write(root, "z.txt", "zz\n");

    let out = f.run("git_diff", json!({"max_bytes": 1024})).await;
    let t = text(&out);
    assert!(out.metadata["truncated"].as_bool().unwrap());
    assert!(
        t.ends_with("[diff truncated for big.txt]\n[diff omitted for z.txt]\n"),
        "{t}"
    );
    assert!(t.len() < 1024 + 100, "{}", t.len());
    hunks_are_whole(&t);
    assert_eq!(out.summary, "git diff: 2 files, +31 \u{2212}31 (truncated)");
    assert_eq!(out.metadata["files"], 2);
    assert_eq!(out.metadata["added"], 31);

    let whole = f.run("git_diff", json!({})).await;
    assert!(!whole.metadata["truncated"].as_bool().unwrap());
    assert_eq!(
        text(&whole).lines().filter(|l| l.starts_with("@@")).count(),
        31
    );
}

#[tokio::test]
async fn log_lists_commits_newest_first_with_a_more_trailer() {
    require_git!();
    let f = Fixture::repo();
    let root = f.root();
    write(root, "src/a.rs", "fn a() { /* 2 */ }\n");
    git_at(
        root,
        &[
            "commit",
            "-q",
            "-am",
            "Second: touch a\n\nWith a body\nof two lines.",
        ],
        "2024-01-03T10:00:00+0000",
    );
    write(root, "README.md", "# third\n");
    git_at(
        root,
        &["commit", "-q", "-am", "Third"],
        "2024-01-04T10:00:00+0000",
    );

    let out = f.run("git_log", json!({})).await;
    let t = text(&out);
    let lines: Vec<&str> = t.lines().collect();
    assert_eq!(lines.len(), 3, "{t}");
    let is_hash = |s: &str| s.len() >= 7 && s.chars().all(|c| c.is_ascii_hexdigit());
    for (line, rest) in lines.iter().zip([
        "2024-01-04 Test User Third",
        "2024-01-03 Test User Second: touch a",
        "2024-01-02 Test User Initial commit",
    ]) {
        let (hash, tail) = line.split_once(' ').unwrap();
        assert!(is_hash(hash), "{line}");
        assert_eq!(tail, rest);
    }
    assert_eq!(out.summary, "git log: 3 commits");
    assert_eq!(out.metadata["count"], 3);
    assert_eq!(out.metadata["more"], false);

    let out = f.run("git_log", json!({"max_count": 2})).await;
    let t = text(&out);
    assert_eq!(t.lines().count(), 3, "{t}");
    assert!(
        t.ends_with("Second: touch a\n[more commits; raise max_count]\n"),
        "{t}"
    );
    assert_eq!(out.summary, "git log: 2 commits, more");
    assert_eq!(out.metadata["more"], true);

    let out = f.run("git_log", json!({"path": "src/a.rs"})).await;
    let t = text(&out);
    assert_eq!(t.lines().count(), 2, "{t}");
    assert!(t.starts_with(&format!(
        "{} 2024-01-03",
        lines[1].split(' ').next().unwrap()
    )));
    assert_eq!(out.summary, "git log: 2 commits touching src/a.rs");

    let out = f
        .run("git_log", json!({"format": "full", "max_count": 2}))
        .await;
    let t = text(&out);
    let expected_tail = "2024-01-03 Test User\n    Second: touch a\n\n    With a body\n    of two lines.\n[more commits; raise max_count]\n";
    assert!(t.ends_with(expected_tail), "{t}");
    assert!(
        t.starts_with(&format!(
            "{} 2024-01-04 Test User\n    Third\n\n",
            lines[0].split(' ').next().unwrap()
        )),
        "{t}"
    );
    assert_eq!(out.metadata["format"], "full");

    let out = f.run("git_log", json!({"path": "nothing.txt"})).await;
    assert_eq!(text(&out), "[no commits]\n");
    assert_eq!(out.summary, "git log: no commits touching nothing.txt");
}

#[tokio::test]
async fn a_directory_that_is_no_repository_says_so() {
    require_git!();
    let f = Fixture::plain();
    for (name, input) in [
        ("git_status", json!({})),
        ("git_diff", json!({"path": "src"})),
        ("git_log", json!({"max_count": 5})),
    ] {
        let out = f.run(name, input).await;
        assert!(out.is_error, "{name}");
        assert!(
            text(&out).contains("is not a git repository"),
            "{name}: {}",
            text(&out)
        );
        assert_eq!(out.summary, "git: not a repository", "{name}");
        assert_eq!(out.metadata["error"], "not_a_repo");
    }

    // Snapshots still tell whether anything changed.
    let before = f.ws.snapshot().await;
    assert!(before.git.is_none());
    assert!(
        before
            .git_error
            .as_deref()
            .unwrap()
            .contains("not a git repository")
    );
    assert_eq!(before.file_count, 2);
    let ev = before.event(SessionId::generate(), None, SnapshotPhase::Start);
    assert_eq!(ev.kind, kinds::WORKSPACE_SNAPSHOT);
    assert_eq!(ev.payload["phase"], "start");
    assert_eq!(ev.payload["git_head"], Value::Null);
    assert_eq!(ev.payload["file_count"], 2);
    assert_eq!(ev.payload["index_hash"].as_str().unwrap().len(), 64);
    assert!(
        ev.payload["git_error"]
            .as_str()
            .unwrap()
            .contains("not a git repository")
    );
    assert!(ev.blob.is_none());
    let same = f.ws.snapshot().await;
    assert_eq!(same.index_hash, before.index_hash);
    assert!(same.changes_since(&before).is_empty());

    write(f.root(), "src/a.rs", "fn a() { 1 }\n");
    let after = f.ws.snapshot().await;
    assert_ne!(after.index_hash, before.index_hash);
    let changes = after.changes_since(&before);
    assert_eq!(changes.changed, ["src/a.rs"]);
    assert!(changes.added.is_empty() && changes.deleted.is_empty());
}

#[tokio::test]
async fn snapshots_bracket_an_edit_with_a_files_changed_outcome() {
    require_git!();
    let f = Fixture::repo();
    let root = f.root();
    let head = git(root, &["rev-parse", "HEAD"]).trim().to_owned();

    let start = f.ws.snapshot().await;
    let git_start = start.git.as_ref().expect("a repository");
    assert_eq!(git_start.head.as_deref(), Some(head.as_str()));
    assert_eq!(git_start.branch.as_deref(), Some("main"));
    assert!(!git_start.dirty);
    assert_eq!(git_start.diff.as_deref(), Some(&b""[..]));
    assert!(git_start.untracked.is_empty());
    assert_eq!(start.file_count, 4);
    let ev = start.event(
        SessionId::generate(),
        Some(AgentId::generate()),
        SnapshotPhase::Start,
    );
    assert_eq!(ev.payload["git_head"], head);
    assert_eq!(ev.payload["branch"], "main");
    assert_eq!(ev.payload["dirty"], false);
    assert_eq!(ev.payload["diff_bytes"], 0);
    assert!(ev.blob.is_none(), "an empty diff is no blob");
    assert!(ev.agent.is_some());

    // A no-op agent: nothing changed.
    let unchanged = f.ws.snapshot().await;
    assert!(unchanged.changes_since(&start).is_empty());
    assert_eq!(
        unchanged.git.as_ref().unwrap().status_hash,
        git_start.status_hash
    );
    assert_eq!(unchanged.index_hash, start.index_hash);

    // An agent that edits, adds, deletes, and rewrites a file at the
    // same size.
    tokio::time::sleep(Duration::from_millis(30)).await;
    write(root, "src/a.rs", "fn a() { 1 }\n");
    write(root, "src/b.rs", "fn c() {}\n");
    write(root, "new.txt", "new\n");
    std::fs::remove_file(root.join("old.txt")).unwrap();
    let end = f.ws.snapshot().await;
    let git_end = end.git.as_ref().unwrap();
    assert_eq!(git_end.head.as_deref(), Some(head.as_str()));
    assert!(git_end.dirty);
    assert_eq!(git_end.untracked, [("new.txt".to_owned(), 4)]);
    let diff = String::from_utf8(git_end.diff.clone().unwrap()).unwrap();
    assert!(diff.contains("+++ b/src/a.rs"), "{diff}");
    assert!(diff.contains("deleted file mode"), "{diff}");
    assert!(!git_end.diff_truncated);
    assert_ne!(git_end.status_hash, git_start.status_hash);
    let ev = end.event(SessionId::generate(), None, SnapshotPhase::End);
    assert_eq!(ev.payload["phase"], "end");
    assert_eq!(ev.payload["dirty"], true);
    assert_eq!(
        ev.payload["untracked"],
        json!([{"path": "new.txt", "bytes": 4}])
    );
    assert_eq!(ev.payload["untracked_count"], 1);
    assert_eq!(ev.payload["diff_bytes"], diff.len());
    assert!(ev.blob.is_some());

    let changes = end.changes_since(&start);
    assert_eq!(changes.changed, ["src/a.rs", "src/b.rs"]);
    assert_eq!(changes.added, ["new.txt"]);
    assert_eq!(changes.deleted, ["old.txt"]);
    let ev = changes.event(SessionId::generate(), None);
    assert_eq!(ev.kind, kinds::OUTCOME);
    assert_eq!(ev.payload["kind"], "files_changed");
    assert_eq!(
        ev.payload["details"]["changed"],
        json!(["src/a.rs", "src/b.rs"])
    );
    assert_eq!(ev.payload["details"]["added"], json!(["new.txt"]));
    assert_eq!(ev.payload["details"]["deleted"], json!(["old.txt"]));
}

#[tokio::test]
async fn a_workspace_below_the_top_sees_only_its_subtree() {
    require_git!();
    let f = Fixture::dirty();
    let sub = Arc::new(Workspace::open(&f.root().join("src")).unwrap());
    let ctx = ToolContext {
        workspace: Some(Arc::clone(&sub)),
        ..f.ctx()
    };
    let out = GitStatus
        .call(&ctx, json!({}), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        text(&out),
        "branch main\nstaged:\n  M a.rs\nunstaged:\n  M a.rs\n[3 changes elsewhere in the repository not shown]\n"
    );
    assert_eq!(out.metadata["outside"], 3);
    let out = GitDiff
        .call(&ctx, json!({}), CancellationToken::new())
        .await
        .unwrap();
    let t = text(&out);
    assert!(t.contains("+++ b/a.rs"), "{t}");
    assert!(!t.contains("README"), "{t}");
    let snap = sub.snapshot().await;
    let g = snap.git.as_ref().unwrap();
    assert!(g.dirty);
    assert!(g.untracked.is_empty(), "{:?}", g.untracked);
    let diff = String::from_utf8(g.diff.clone().unwrap()).unwrap();
    assert!(
        diff.contains("+++ b/a.rs") && !diff.contains("README"),
        "{diff}"
    );
}

#[test]
fn git_tool_specs_golden() {
    let specs: Vec<ToolSpec> = git_tools().iter().map(|t| t.spec()).collect();
    insta::assert_json_snapshot!("git_tool_specs", specs);
}
