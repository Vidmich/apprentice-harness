# M01-02 — Workspace model and path sandbox

Status: todo
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

- [ ] `resolve` rejects `../` escapes, absolute paths outside root, and
      symlinks that leave the root; accepts paths inside (case-insensitive on
      Windows).
- [ ] Walker honours `.gitignore` in nested dirs and `.harness/ignore`.
- [ ] Index of a 50k-file tree builds in < 2 s on the reference machine.
- [ ] `workspace.add` twice for the same root returns the same id;
      `workspace.info` reports git head when the root is a repo.
- [ ] Migration v002 applies to an M00 database without data loss.

## Verification

Unit tests with temp trees (including symlinks where the OS allows creating
them without elevation — on Windows mark that test `#[ignore]` unless
developer mode is on).

## Notes

- Keep `display()` paths POSIX-style for the mentor regardless of OS;
  convert back in `resolve`.
