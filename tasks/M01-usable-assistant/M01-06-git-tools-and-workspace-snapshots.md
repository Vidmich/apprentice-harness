# M01-06 — Git tools and workspace snapshots

Status: todo
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

- [ ] Golden tests for each tool against a fixture repo created in the test
      (init, commit, modify, add untracked).
- [ ] Snapshot start/end pair around an agent that edits a file yields a
      `files_changed` outcome naming that file; no-op agent yields empty.
- [ ] Non-git directory: tools report unavailability gracefully; snapshots
      still record index hashes.
- [ ] Diff truncation never splits a hunk header.

## Verification

`cargo test -p apprentice-core tools::git::` (requires `git` on PATH; skip
with a clear message if absent).

## Notes

- `git diff` of a dirty tree at start is what a trainer needs to reproduce
  the exact pre-task state; combined with `git_head` it is sufficient for
  tracked files. Untracked files are listed, not copied (size guard).
