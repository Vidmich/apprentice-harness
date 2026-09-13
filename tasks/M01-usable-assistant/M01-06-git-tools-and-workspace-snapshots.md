# M01-06 — Git tools and workspace snapshots

Status: done
Depends on: M01-01, M01-02, M01-05
Size: S

## Goal

Read-only git tools for the mentor (`git_status`, `git_diff`, `git_log`)
that return structured, compact output, plus automatic **workspace
snapshots** at agent start and end so every trajectory records what the
code looked like before and after — the basis for outcome measurement and
replay.

## Context

SPEC §9 (workspace snapshot references in traces), §12 (task outcomes).
Write operations (commit, branch) are deliberately left to the `shell`
tool under permissions; the user's global rule is that the harness does not
commit unless asked.

## Scope

In: three read-only git tools, snapshot capture, `workspace.snapshot`
events with diff blobs, non-git workspace fallback.
Out: git write tools, git library bindings (shell out to `git`).

## Design

Git is invoked as a subprocess (`git -C <root> ...`) with
`GIT_OPTIONAL_LOCKS=0`, `LC_ALL=C`, 30 s timeout; missing `git` → tools
report "git not available".

### Tools (all Risk::ReadOnly)

- `git_status {}` → porcelain v2 parsed into
  `branch, upstream, ahead/behind, staged: [...], unstaged: [...], untracked: [...]`
  rendered as compact text (paths grouped by state). Summary:
  `git: main, 3 modified, 1 untracked`.
- `git_diff {path?, staged?: bool, base?: ref, max_bytes?: 64k}` → unified
  diff text (`git diff --no-color --no-ext-diff`), truncated per-file with
  `[diff truncated for path]` markers rather than mid-hunk.
- `git_log {max_count?: 20, path?, format?: "oneline"|"full"}` →
  `--format=%h %ad %an %s` with `--date=short`.

### Snapshots

At `agent.started` and `agent.finished` the runtime (M01-08) calls
`Workspace::snapshot()`:

```
{ git_head: Option<sha>, branch: Option<String>, dirty: bool,
  status_hash: sha256 of porcelain output, diff_blob: Option<BlobId> /* git diff HEAD (tracked) + list of untracked files with sizes */,
  file_count: usize, index_hash: sha256 of the file index listing }
```

Recorded as `workspace.snapshot {phase: start|end, ...}` events. For non-git
workspaces: `git_head = None`, `diff_blob = None`, but `index_hash` and
`file_count` still allow "did anything change" checks. Cap the diff at
16 MiB (larger → store head/tail and mark truncated).

Derived at end: `outcome{kind: "files_changed", details: {changed: [...],
added: [...], deleted: [...]}}` computed from the two snapshots (M01-15
adds more outcome kinds).

## Acceptance

- [x] Golden tests for each tool against a fixture repo created in the test
      (init, commit, modify, add untracked). —
      `tools::git::tests::{status_groups_paths_by_state,
      status_of_a_clean_tree_and_of_a_fresh_repo,
      diff_shows_the_working_tree_the_index_or_a_base,
      log_lists_commits_newest_first_with_a_more_trailer,
      a_workspace_below_the_top_sees_only_its_subtree}` (the fixture:
      init, commit, stage, modify again, `git mv`, untracked file) and
      the `git_tool_specs` insta golden.
- [x] Snapshot start/end pair around an agent that edits a file yields a
      `files_changed` outcome naming that file; no-op agent yields empty. —
      `snapshots_bracket_an_edit_with_a_files_changed_outcome` (unit) and
      `tests/snapshots.rs` (through the trace store: two
      `workspace.snapshot` events, the diff blob, the `outcome` event).
- [x] Non-git directory: tools report unavailability gracefully; snapshots
      still record index hashes. —
      `a_directory_that_is_no_repository_says_so`.
- [x] Diff truncation never splits a hunk header. —
      `diff_truncation_never_splits_a_hunk` (checks every hunk against
      its header's line counts), `diff::tests`.

## Verification

`cargo test -p apprentice-core tools::git::` (requires `git` on PATH; skip
with a clear message if absent).

## Notes

- `git diff` of a dirty tree at start is what a trainer needs to reproduce
  the exact pre-task state; combined with `git_head` it is sufficient for
  tracked files. Untracked files are listed, not copied (size guard).

## Completion notes (2026-09-12)

Module layout (`cargo test -p apprentice-core tools::git::` and
`workspace::`; every git test skips with "git is not on PATH" when it
must):

- `workspace/git/` — `git_head` (the `.git` reader of M01-02, unchanged)
  plus `run.rs`: `Repo::open(root)` (`rev-parse --show-prefix`, so a
  workspace below the top of a work tree works: status is restricted to
  the subtree and made root-relative, diffs use `--relative`) and
  `Repo::run(args, max_stdout)` — `git -C <root> --no-pager ...` with
  `GIT_OPTIONAL_LOCKS=0`, `GIT_TERMINAL_PROMPT=0`, `LC_ALL=C`, no
  window on Windows, both pipes read concurrently into a bounded
  capture, `GIT_TIMEOUT` 30 s then kill. `GitError::{NotAvailable,
  NotARepo, TimedOut, Failed, Io}`; a missing binary is the `NotFound`
  of the spawn, nothing is probed up front. `status.rs` parses
  `--porcelain=v2 -z --branch` (headers, ordinary, rename/copy with the
  original path, unmerged, untracked) into `Status`.
- `workspace/snapshot.rs` — `Workspace::snapshot()` (async, on an
  `Arc`): git status (`-uall`) and `git diff HEAD --relative -- .`
  capped at 16 MiB (head/tail, `[... N bytes omitted]`,
  `diff_truncated`) alongside a fresh index build on the blocking pool
  (the index is refreshed as a side effect). `Snapshot::event(session,
  agent, phase)` builds the `workspace.snapshot` event: payload
  `{phase, git_head, branch, dirty, status_hash, diff_bytes,
  diff_truncated, untracked: [{path, bytes}] (≤ 1000), untracked_count,
  file_count, index_hash, index_truncated, took_ms, git_error?}`, the
  diff as the event blob (`text/x-diff`, omitted when empty).
  `Snapshot::changes_since(&start)` merges the two index listings
  (path, size, mtime at nanosecond resolution — `FileEntry` gained
  `mtime_ns` for this) into `FilesChanged {changed, added, deleted}`;
  `FilesChanged::event` is the `outcome {kind: files_changed, details:
  {changed, added, deleted, counts, truncated?}}` event.
- `tools/git/` — `git_status {}`, `git_diff {path?, staged?, base?,
  max_bytes?}`, `git_log {max_count?, path?, format?}`, all
  `Risk::ReadOnly`, tag `git`, in `builtin_tools()`. Paths go through
  the sandbox (`target`), refs must not look like options (schema
  pattern and a check), pathspecs always follow `--`. `diff.rs` cuts a
  diff per file: whole files while they fit, then whole hunks, then
  `[diff truncated for <path>]` and `[diff omitted for <path>]` per
  remaining file (50 listed, then `[... N more files omitted]`); counts
  (`files`, `+added −removed`) are over the whole diff. Summaries:
  `git: main, 2 staged, 3 modified, 1 untracked` / `git: main, clean`,
  `git diff: 2 files, +3 −1 (truncated)` / `git diff: no unstaged
  changes`, `git log: 20 commits touching src, more`.

Deviations / decisions:

- `git_status` shows counts in its metadata and the paths in the text
  (grouped `staged:` / `unstaged:` / `unmerged:` / `untracked:` with
  git's letters, `R old -> new`), not JSON lists: the text is the
  mentor's contract and the payload stays small. Groups are cut at 500
  paths.
- `dirty` means tracked changes; untracked files are counted and listed
  separately (as `git describe --dirty` sees it).
- The untracked list lives in the snapshot payload, not in the diff
  blob, so the blob is a clean `git diff` (applies with `git apply`).
  Binary changes are not included (`--binary` would blow up the blob);
  the diff says `Binary files differ`.
- An unborn `HEAD` (fresh `git init`) yields no diff; the index half
  still records the tree.
- `git_log` uses `-z` so commits are counted exactly; it asks for one
  more than `max_count` and says `[more commits; raise max_count]`.
  `format: full` indents the message four spaces (`%w(0,4,4)%B`),
  blank lines trimmed.
- `git_diff` keeps up to 8 MiB of git's output; a diff past that is
  cut by the capture and the summary counts get a `≥` (`partial` in
  metadata).
- The runtime hook-up (snapshot at `agent.started`/`agent.finished`,
  the `outcome` after) is M01-08's; `tests/snapshots.rs` runs that
  sequence against the store.
