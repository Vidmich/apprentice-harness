# M01-03 — File tools: read, write, edit, list, glob

Status: todo
Depends on: M01-01, M01-02
Size: M

## Goal

The core file tools the mentor uses on a workspace, with mentor-oriented
descriptions, precise line-numbered output, safe atomic writes, an exact
string-replacement edit, and traces that capture full raw outputs.

## Context

SPEC §3.1 Tool system (coding tools first). Output formats matter twice:
they are what the mentor reads now, and the raw blobs are what the
apprentice compressor learns from in M03/M06 — so formats must be stable
and machine-parseable.

## Scope

In: `read_file`, `write_file`, `edit_file`, `list_dir`, `glob`, output
formats, encoding handling, size limits, diff generation for edits.
Out: search (M01-04), shell (M01-05), git (M01-06), multi-file patches.

## Design

### `read_file` — Risk::ReadOnly

Input: `{path: string, offset?: int (1-based line), limit?: int (lines)}`.
Output text: `<n>\t<line>` per line (tab-separated line number), default
limit 2000 lines and 200 KiB; over-limit → truncated with trailer
`[truncated: showing lines A–B of N; call again with offset]`. Binary
detection (NUL in first 8 KiB) → error result "binary file (size, sniffed
type)". Encoding: UTF-8, fall back to lossy with a note. Metadata: `{lines,
bytes, language, truncated}`. Summary: `read src/x.rs lines 1–212 of 212`.

### `write_file` — Risk::Write

Input: `{path, content}`. Creates parent dirs. Atomic: write temp in same
dir → fsync → rename. If the file exists and was not read in this agent's
session (tracked in `ToolContext` via a per-agent "seen files" set with
content hash), warn in the result (`note: overwrote a file you had not
read`) — but still do it (permissions are the gate, not the tool). Output:
`{bytes_written, created: bool}`; the result blob includes a unified diff vs
the previous content (empty for new files) so traces carry the change.
Summary: `wrote src/x.rs (+40 −3)`.

### `edit_file` — Risk::Write

Input: `{path, old_string, new_string, replace_all?: bool}`. Exact
substring match on the current content; `old_string` must occur exactly
once unless `replace_all`; zero matches → error result listing the closest
line (by fuzzy ratio) to help the mentor fix its quote; multiple → error
listing the line numbers. Preserves original line endings and BOM. Writes
atomically as above. Output: unified diff of the change + `{replacements}`.
Summary: `edited src/x.rs (+2 −1)`.

### `list_dir` — Risk::ReadOnly

Input: `{path?: string (default "."), depth?: int (default 1, max 4)}`.
Output: tree listing using the workspace walker (ignore rules applied),
`dir/` suffix for directories, sizes for files, max 2000 entries then
`[+N more]`. Summary: `listed src/ (34 entries)`.

### `glob` — Risk::ReadOnly

Input: `{pattern: string, path?: string}`; gitignore-style globs via
`globset`; matches from the file index (M01-02), sorted by mtime desc, max
1000 results. Output: one path per line. Summary: `glob **/*.rs → 87 files`.

### Shared

- All paths resolved through `Workspace::resolve` (M01-02); `Outside` →
  the permission engine decides (M01-07) whether to allow (then the tool
  operates on the absolute path).
- Line endings: tools never rewrite CRLF↔LF except where the mentor's
  `new_string` introduces them.
- Diffs use the `similar` crate, unified format, 3 lines of context, paths
  in `display()` form.

## Acceptance

- [ ] Golden tests for each output format (snapshot files) including
      truncation trailers and error results.
- [ ] `edit_file` ambiguity and no-match paths produce the specified helpful
      errors; CRLF file edit preserves CRLF; BOM preserved.
- [ ] `write_file` is atomic (simulate failure between temp write and
      rename → original intact).
- [ ] Binary and non-UTF-8 files handled as specified.
- [ ] Traces: `tool.result` blob for an edit contains the unified diff;
      `tool.call` blob contains the exact input.

## Verification

`cargo test -p apprentice-core tools::file::`; manual session in the GUI
asking the mentor to read, edit and create files in a scratch repo.

## Notes

- Tool descriptions are part of the cached prefix; changing wording
  invalidates the cache for all sessions. Keep them stable once M01 ships;
  version them via the protocol directory in M03.
