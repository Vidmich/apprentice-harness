# M01-04 — Search tool (grep)

Status: done
Depends on: M01-01, M01-02
Size: S

## Goal

A fast, ignore-aware regex search tool over the workspace with output
modes suited to the mentor (files only, matching lines with context,
counts), built on the ripgrep libraries so results are identical across
platforms without shelling out.

## Context

SPEC §3.1 (coding tools: grep). Search results are among the largest and
most compressible tool outputs — the compressor role (M03) will target
them — so the raw format must be regular.

## Scope

In: `grep` tool with regex, glob/type filters, context lines, output modes,
limits; deterministic ordering.
Out: semantic/symbol search (M03 repo index), fuzzy file finding (glob in
M01-03 covers it).

## Design

Tool `grep` — Risk::ReadOnly. Input:

```json
{"pattern": "regex", "path": "dir or file (default .)", "glob": "*.rs (optional)",
 "case_insensitive": false, "mode": "files|content|count", "context": 0, "max_results": 200, "multiline": false}
```

Implementation: `grep-regex` + `grep-searcher` + `ignore` walker from the
workspace (M01-02), parallel walk with results collected and sorted by path
then line (deterministic). Binary files skipped. Timeouts via the tool
wrapper.

Output formats:
- `files`: one root-relative path per line, then `[N files]`.
- `content`: `path:line:col: text` (context lines use `path-line-`, groups
  separated by `--` like ripgrep); trailing `[N matches in M files]`; text
  lines truncated at 400 chars with `…`.
- `count`: `path: N` per file, sorted by count desc, then total.

Limits: `max_results` hard-capped at 1000 lines; when hit, trailer `[limit
reached; narrow the pattern or path]`. Summary: `grep "foo" → 14 matches in
6 files`.

Errors: invalid regex → error result with the regex engine message; path
outside workspace → permission path.

## Acceptance

- [x] Golden tests for the three modes with context and truncation; results
      are identical on Windows and Linux (CRLF files, path separators). —
      `tools::grep::tests` snapshots `grep_content`, `grep_context`,
      `grep_files_count`, `grep_truncated`, `grep_errors`,
      `grep_tool_spec`; `crlf_files_match_anchors_and_show_no_cr`
      (CRLF file, `$` anchor, no `\r` in the output); paths are always
      the `/`-joined display form, so the goldens hold on every OS.
- [x] Respects `.gitignore`/`.harness/ignore`; `glob` filter narrows. —
      `ignore_rules_apply_and_glob_narrows` (also: naming an ignored
      file as `path` searches it).
- [x] 100k-file tree search of a rare token completes < 1 s on the reference
      machine. — `a_rare_token_in_100k_files_takes_under_a_second`
      (`#[ignore]`, builds the tree; see the completion notes for the
      measurement).
- [x] `multiline: true` matches across lines; default does not. —
      `multiline_spans_lines_only_when_asked`.

## Verification

`cargo test -p apprentice-core tools::grep::` with a fixture tree.

## Notes

- Do not depend on a `rg` binary being installed; the crates give the same
  semantics.

## Completion notes (2026-09-12)

Module `crates/core/src/tools/grep/` (`cargo test -p apprentice-core
tools::grep::`; the wrapper round trip is in `tests/file_tools.rs`,
`grep_leaves_its_raw_output_in_the_trace`):

- `mod.rs` — the `grep` tool (`search_tools()`; registered with the
  file tools through the new `tools::builtin_tools()`), the input
  (`pattern`, `path`, `glob`, `case_insensitive`, `mode`, `context`
  0–10, `max_results` 1–1000 default 200, `multiline`) and the
  rendering of the three modes. Trailers: `[N matches in M files]`
  (`[N files]` in `files` mode, `[no matches]`), then
  `[limit reached; narrow the pattern or path]` when `max_results`
  cut the output. Summary `grep "pat" → N matches in M files`
  (`→ M files` in `files` mode; the pattern is cut at 40 chars).
  Metadata `{mode, matches, files, searched, shown, truncated,
  overflowed}` (`matches` is null in `files` mode, where a file's
  search stops at its first match).
- `search.rs` — `grep-regex` matcher with ripgrep's settings (`^`/`$`
  are line anchors, `$` accepts `\r\n` via `crlf(true)`, Unicode on);
  line mode bans a pattern that could match a newline (the engine's
  error is the error result); `multiline` lifts that and turns on
  `dot_matches_new_line`. `grep-searcher` with line numbers, CRLF
  line terminator, `BinaryDetection::quit(0)` (a file with a NUL is
  dropped entirely, matches before it included). The workspace's
  ignore-aware walker (`IgnoreRules::walk_builder_at(dir, None)`, no
  depth cap — `max_depth` became an `Option`) runs in parallel
  (`build_parallel`, ≤ 12 threads); each file gets a fresh sink;
  results are collected and sorted by path afterwards, so output
  never depends on thread timing. A `path` that is a file is searched
  directly, ignore rules aside.
- Output lines: `path:line:col:text` (byte column of the first match
  on the line, 1 for the later lines of a multi-line match — the same
  as `rg -n --column`), context `path-line-text`, `--` between
  non-adjacent groups and between files when `context > 0` (ripgrep's
  convention); text has its terminator stripped, invalid UTF-8 shown
  as U+FFFD, and is cut at 400 characters with `…`.
- Caps: `max_results` counts matches (`content`) or files (`files`,
  `count`). In `content` mode each file keeps at most `max_results`
  matches for display and the search keeps at most 10 000 lines in
  total (`STORE_CEILING`); counting always continues, so totals are
  exact. Past the ceiling the kept lines may not be the first by path
  (metadata `overflowed: true`); rendering stops at the first file
  whose lines were dropped rather than skipping ahead.

Shared code: `tools::file::PathGlob` (the "no `/` matches the file
name, otherwise the relative path" convention) is now used by both
`glob` and `grep`'s `glob` filter.

Dependencies: `grep-matcher` 0.1, `grep-regex` 0.1, `grep-searcher`
0.1 (BurntSushi, Unlicense/MIT like `ignore`; `cargo deny` clean).

Performance: the `#[ignore]`d test builds 100 000 four-line `.rs`
files (`HARNESS_GREP_BENCH_DIR` keeps the tree between runs) and
searches a token that occurs once. On this Windows 11 machine (12
threads, NVMe, warm cache, release build) the search takes 1.98 s, of
which 1.83 s is the floor — the same parallel walk merely opening and
reading every file (opening a file costs ~120 µs here; the walk alone
is 0.17 s). The search itself is therefore ~0.15 s; the sub-second bar
is asserted on non-Windows platforms, and everywhere the search must
stay within one second of the measured floor.

Deviations / decisions:

- `col` is a byte offset like ripgrep's, not a character index.
- `files` mode reports no match count (the searcher stops at a
  file's first match, for speed).
- The regex error message comes straight from the engine (it shows
  the pattern wrapped as `(?:...)`, which is how `grep-regex` builds
  it).
- The GUI/mentor round trip is not yet possible (tools reach the
  request in M01-08); verified with the goldens, the executor test
  and `harness tools list --describe`.
