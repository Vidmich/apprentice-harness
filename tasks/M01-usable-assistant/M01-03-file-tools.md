# M01-03 — File tools: read, write, edit, list, glob

Status: done
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

- [x] Golden tests for each output format (snapshot files) including
      truncation trailers and error results. — `tools::file::tests`
      snapshots `read_basic`, `read_paged`, `read_errors`,
      `read_encodings`, `write_new`, `write_overwrite_unread`,
      `edit_single`, `edit_replace_all`, `edit_errors`, `list_trees`,
      `glob_matches`, `file_tool_specs` (the descriptions, since they
      are cached-prefix material); caps in
      `read_file_truncates_at_the_line_and_byte_caps`,
      `list_dir_caps_the_entries`, `glob_caps_the_results`.
- [x] `edit_file` ambiguity and no-match paths produce the specified helpful
      errors; CRLF file edit preserves CRLF; BOM preserved. —
      `edit_file_no_match_and_ambiguity_errors`,
      `edit_file_preserves_crlf_and_bom`.
- [x] `write_file` is atomic (simulate failure between temp write and
      rename → original intact). —
      `atomic::tests::a_failure_before_the_rename_leaves_the_original_and_no_temp_file`.
- [x] Binary and non-UTF-8 files handled as specified. —
      `read_file_error_results` (binary → error result with size and
      sniffed type), `read_file_decodes_lossy_utf16_bom_and_crlf`,
      `edit_file_no_match_and_ambiguity_errors` (binary / lossy / UTF-16
      refused by `edit_file`).
- [x] Traces: `tool.result` blob for an edit contains the unified diff;
      `tool.call` blob contains the exact input. — integration test
      `file_tools::edit_leaves_the_exact_input_and_the_diff_in_the_trace`
      (also `write_file_notes_unread_overwrites_and_keeps_the_diff`).

## Verification

`cargo test -p apprentice-core tools::file::`; manual session in the GUI
asking the mentor to read, edit and create files in a scratch repo.

## Notes

- Tool descriptions are part of the cached prefix; changing wording
  invalidates the cache for all sessions. Keep them stable once M01 ships;
  version them via the protocol directory in M03.

## Completion notes (2026-09-12)

Module `crates/core/src/tools/file/` (`cargo test -p apprentice-core
tools::file::` plus `tests/file_tools.rs` through the executor):

- `mod.rs` — `file_tools()` (the five tools, registered by
  `AppState::open_with`, so `harness tools list` shows them), the caps
  (`READ_MAX_LINES` 2000, `READ_MAX_BYTES` 200 KiB, `LIST_MAX_ENTRIES`
  2000, `LIST_MAX_DEPTH` 4, `GLOB_MAX_RESULTS` 1000), `target()` (path →
  `Workspace::resolve`; `Outside` → `ToolError::Denied`, `Invalid` →
  `InvalidInput`, no workspace → `Failed`), `blocking()` (every tool body
  runs on `spawn_blocking`).
- `read.rs` — `<n>\t<line>`, `offset`/`limit`, both caps with the
  `[truncated: showing lines A–B of N; call again with offset B+1]`
  trailer, `[empty file]`, error results for missing / directory /
  binary (`is a binary file (N bytes, image/png)`) / offset past the end;
  `[note: ...]` header for lossy UTF-8 and UTF-16 (decoded via BOM).
  Metadata `{lines, bytes, language, truncated, offset, end, encoding,
  bom, line_endings}`. Records the file in `SeenFiles`.
- `write.rs` — atomic write, parents created, output = header
  (`wrote p (N bytes, new file, L lines)` / `wrote p (N bytes, +a −r)`),
  optional `note: overwrote a file you had not read` /
  `note: the file changed on disk since you read it`, then the unified
  diff against the previous content (none for new files; a placeholder
  when the old content was binary). Creating a file invalidates the
  workspace index so `glob` sees it at once.
- `edit.rs` — exact match, once or `replace_all`; zero matches → error
  listing the closest line (substring match first, else `similar`'s
  fuzzy ratio ≥ 0.5) and a quoting hint; several → error listing the
  line numbers (deduplicated, first 10). A CRLF file quoted with LF
  (what `read_file` shows) is matched and diffed as LF and written back
  CRLF; mixed files are edited byte for byte; the BOM is kept. Binary,
  lossy-UTF-8 and UTF-16 files are refused (use `write_file`).
- `list.rs` — `Workspace::walker_at(dir, depth)`: `name/`, `name\t<bytes>`,
  `name@` (symlink), two spaces per level, `[+N more]`, `[empty
  directory]`. Naming an ignored directory (`target`) lists it anyway
  (`IgnoreRules::walk_builder_at` drops the built-in/`.harness/ignore`
  filter when the start directory itself is ignored).
- `glob.rs` — `globset` with `literal_separator`; a pattern without `/`
  matches file names at any depth, otherwise the path relative to
  `path`; matches from `Workspace::index()`, mtime desc then path,
  workspace-relative output, `[no files match P]`, `[truncated: showing
  1000 of N matches]`, a note when the index itself was truncated.
- `atomic.rs` (`write_atomic`: temp file in the same directory,
  `create_new`, fsync, permissions copied on Unix, rename, directory
  fsync on Unix; the temp file is removed on any failure), `diff.rs`
  (`unified_diff` via `similar`, 3 lines of context, `a/<p>`/`b/<p>`
  headers, +/− counts; `closest_line`), `text.rs` (NUL sniff in the
  first 8 KiB, magic-number/extension media types, BOM and UTF-16
  decoding, line-ending detection).

Tool system additions: `SeenFiles` (path → SHA-256 at last read/write)
in `ToolContext::seen`, supplied per agent through
`Executor::with_seen_files` (a fresh set per executor otherwise — M01-08
must hold one per agent across steps). `Workspace::invalidate_index()`,
`Workspace::walker_at()`, `IgnoreRules::walk_builder_at()`.

Dependencies: `similar` 2.7 and `globset` 0.4 (both already in the lock
via `insta` / `ignore`; `cargo deny` unchanged).

Deviations / decisions:

- `Outside` paths are `denied` results today (the wrapper records them
  like any denial); M01-07's gate gets to ask the user before the tool
  runs, and can then hand the tool an allowed absolute path.
- `edit_file` carries the same unread/changed notes as `write_file`
  (the spec only asked for `write_file`); neither blocks.
- `list_dir` prints sizes in bytes (machine-parseable) rather than
  human units; the root itself is not printed.
- `glob` results are workspace-relative even when `path` narrows the
  search, so they feed straight into `read_file`.
- The GUI/mentor round trip is not yet possible (tools reach the
  request in M01-08); verified with the goldens, the executor tests and
  `harness tools list --describe`.
