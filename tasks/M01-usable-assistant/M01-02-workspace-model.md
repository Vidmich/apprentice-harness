# M01-02 — Workspace model and path sandbox

Status: done
Depends on: M00-03, M00-06
Size: S

## Goal

A `Workspace` is a registered root directory (usually a repo) that sessions
attach to. It provides path resolution and sandboxing for tools, ignore
rules, a `.harness/` directory for per-workspace config and instructions,
and a lightweight file index used by tools and later by the context
selector (M03).

## Context

SPEC §3.1 (workspaces in GUI, workspace-layer config), §13 (workspaces UI).
Every file tool must be confined to the workspace unless the user explicitly
allows a path outside it (permission engine, M01-07).

## Scope

In: workspace registry (persisted), path canonicalisation and containment
checks, ignore rules, `.harness/` conventions, file listing/index with
cheap refresh, RPC `workspace.*`.
Out: multi-root workspaces (later), file watching beyond a manual refresh.

## Design

### Registry

Table in `traces.sqlite` (add migration v002):
`workspaces(id TEXT PK, root TEXT UNIQUE, name TEXT, created_at, last_used_at, settings_json)`.
`sessions.workspace_path` stays (historical snapshot) and gains
`workspace_id` (nullable) in the same migration.

RPC: `workspace.add {root, name?}`, `workspace.list`, `workspace.remove {id}`
(does not delete files or traces), `workspace.info {id}` → root, name,
file_count, git_head?, has_instructions, config overrides.

### Paths

```rust
impl Workspace {
    pub fn resolve(&self, user_path: &str) -> Result<PathBuf, PathError>;   // relative to root; absolute allowed only if inside root
    pub fn contains(&self, abs: &Path) -> bool;                             // after canonicalize + symlink resolution
    pub fn display(&self, abs: &Path) -> String;                            // root-relative with '/' separators for the mentor
}
```

Symlinks pointing outside the root resolve to outside → `PathError::Outside`
(the permission engine may then ask the user). On Windows, compare
case-insensitively and normalise `\\?\` prefixes and drive letter case.

### Ignore rules

Combine, in order: built-in defaults (`.git/`, `node_modules/`, `target/`,
`dist/`, `.harness/blobs`, binaries by extension), `.gitignore` chain (via
the `ignore` crate), `.harness/ignore` (same syntax). Tools that list or
search use `Workspace::walker()` which applies these; `read_file` on an
ignored path still works when the mentor names it explicitly (ignore rules
are for discovery, not access control). Access control is the sandbox +
permissions.

### `.harness/` directory

```
.harness/config.toml     workspace config layer (M00-03)
.harness/HARNESS.md      project instructions injected into the system prompt (M01-09)
.harness/ignore          extra ignore patterns
.harness/permissions.toml  per-workspace permission rules (M01-07)
```

Add `.harness/` to the workspace's own ignore defaults except `HARNESS.md`
(the mentor may read it).

### File index

`Workspace::index()` → `Arc<FileIndex>`: sorted list of root-relative paths
with size, mtime, language (by extension via a small table). Built on first
use with the walker, refreshed on demand (`workspace.refresh`) or when older
than 30 s at next use. Cap at 200k files with a warning. Used by
`list_dir`/`glob` (M01-03) and later by the context selector.

## Acceptance

- [x] `resolve` rejects `../` escapes, absolute paths outside root, and
      symlinks that leave the root; accepts paths inside (case-insensitive on
      Windows). — `resolve_keeps_paths_inside_the_root`,
      `windows_paths_compare_case_insensitively_and_without_verbatim_prefix`,
      `junctions_that_leave_the_root_resolve_outside` (Windows, no
      privilege needed), `symlinks_that_leave_the_root_resolve_outside`
      (`#[ignore]` on Windows).
- [x] Walker honours `.gitignore` in nested dirs and `.harness/ignore`. —
      `walker_honours_gitignore_chain_harness_ignore_and_builtins` (also
      `.git/info/exclude` and the built-ins).
- [x] Index of a 50k-file tree builds in < 2 s on the reference machine. —
      `fifty_thousand_file_tree_indexes_in_under_two_seconds` (`--ignored`):
      187 ms for 50 000 files / 1 000 dirs.
- [x] `workspace.add` twice for the same root returns the same id;
      `workspace.info` reports git head when the root is a repo. —
      `add_is_idempotent_and_info_reports_the_tree`; checked live with
      `harness ws info .` on this repo (`main @ a33b84b…`).
- [x] Migration v002 applies to an M00 database without data loss. —
      `migration_v002_applies_to_an_m00_database` (integration, builds a
      v1 file from `v001.sql`) and `schema::tests::v1_database_migrates_without_data_loss`.

## Verification

Unit tests with temp trees (including symlinks where the OS allows creating
them without elevation — on Windows mark that test `#[ignore]` unless
developer mode is on).

## Notes

- Keep `display()` paths POSIX-style for the mentor regardless of OS;
  convert back in `resolve`.

## Completion notes (2026-09-12)

Module `crates/core/src/workspace/`:

- `mod.rs` — `Workspace` (`open(root)`, `from_record`, `id()`, `root()`,
  `name()`, `.harness/` accessors incl. `instructions()`, `resolve`,
  `contains`, `display`, `ignore_rules`/`reload_ignore_rules`, `walker`,
  `is_ignored`, `index`/`cached_index`/`refresh`, `git_head`),
  `WorkspaceError { Root, Path, Trace }` → RPC mapping, the `.harness/`
  file-name constants.
- `paths.rs` — canonical roots (symlinks resolved; on Windows `\\?\`
  stripped and drive letter upper-cased), lexical `.`/`..` folding, then
  canonicalisation of the longest existing prefix so a file about to be
  created still resolves and a symlink/junction anywhere in the path is
  followed; component-wise containment, case-insensitive on Windows.
  `PathError { Outside, Invalid, Io }` with `kind()`.
- `rules.rs` — `IgnoreRules`: `BUILTIN_IGNORES` (VCS dirs, `node_modules/`,
  `target/`, `dist/`, `__pycache__/`, `.venv/`, `.harness/*` except
  `HARNESS.md`, binaries by extension) + `.harness/ignore`, compiled once
  as one gitignore set (last match wins, so `!pattern` in `.harness/ignore`
  can un-ignore a built-in); `walk_builder()` = `ignore::WalkBuilder` with
  `.gitignore` chain + `.git/info/exclude` (`require_git(false)`), dotfiles
  included, no symlink following, sorted, plus a `filter_entry` for the
  rules above.
- `index.rs` — `FileIndex` (parallel walk; `FileEntry { path, size,
  mtime, language }` sorted by path; `get`, `under(dir)`, `directories`,
  `is_truncated` at `MAX_INDEX_FILES = 200_000` with a warning, `age`,
  `is_stale` at `INDEX_TTL = 30 s`), `language_of` table.
- `git.rs` — `git_head(root) -> Option<GitHead { commit, branch }>`
  reading `.git/HEAD`, loose refs, `packed-refs`, `gitdir:` files and
  `commondir` (worktrees); no git binary or libgit2.
- `manager.rs` — `Workspaces` (registry rows + one open `Arc<Workspace>`
  per id so the index is shared): `add` (idempotent by canonical root),
  `get`, `record`, `open_root` (registered handle or ad-hoc), `list`,
  `remove`, `touch`.
- `rpc.rs` — `WorkspaceService`: `workspace.add/list/remove/info/refresh`;
  `info`/`refresh` build the index on `spawn_blocking` and report
  `file_count`, `index_truncated`, `index_age_s`, `git_head`, `git_branch`,
  `has_instructions`, `has_config`, `has_ignore_file`, `config_overrides`
  (dotted keys whose source is the workspace layer).

Store: migration `v002.sql` (`workspaces` table, `sessions.workspace_id`,
indexes); `TraceStore::{add_workspace, get_workspace, find_workspace,
list_workspaces, touch_workspace, rename_workspace, set_workspace_settings,
remove_workspace}`; `NewSession`/`SessionRecord` gained `workspace_id`;
`WorkspaceId`/`WorkspaceRecord` in `trace`. `session.create` with a
`workspace` now registers (or finds) it, stores the canonical root as
`workspace_path` and links `workspace_id`; a non-directory is
`invalid_params`. `AppState::workspaces()`.

Wire: `WorkspaceSummary` (types), `WorkspaceAddParams`, `WorkspaceIdParams`,
`WorkspaceListResult`, `WorkspaceRemoveResult { sessions_unlinked }`,
`WorkspaceInfoResult`; snapshots `workspace_{add,list,remove,info}`;
`api.ts` mirror (23 methods). CLI `harness workspace|ws add [DIR] [--name]
| list | remove ID | info [ID_OR_DIR] | refresh [ID_OR_DIR]` (a directory
target is registered on the way).

Tools: `ToolContext::workspace` / `Executor::with_workspace` now carry
`Option<Arc<Workspace>>` instead of a bare path.

Deviations / decisions:

- `workspace.remove` returns the number of sessions unlinked (they keep
  `workspace_path`).
- Ad-hoc roots (`Workspaces::open_root` on an unregistered directory) are
  not cached; only registered workspaces share a handle.
- `.harness/ignore` cannot un-ignore something a `.gitignore` excludes
  (the two sets are evaluated independently by the walker).
- `Workspace::index()` is synchronous; RPC and (later) tools call it from
  `spawn_blocking`.
- No file watching; staleness is the 30 s TTL plus `workspace.refresh`.

Open for later tasks: M01-03 file tools use `resolve`/`display`/`index`;
M01-06 records `workspace.snapshot` from `git_head`; M01-07 turns
`PathError::Outside` into a permission question; M01-08 attaches the
session's workspace (`Workspaces::get(session.workspace_id)` or
`open_root(workspace_path)`) to the executor; M01-09 injects
`instructions()`.
