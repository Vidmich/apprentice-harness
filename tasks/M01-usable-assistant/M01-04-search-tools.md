# M01-04 — Search tool (grep)

Status: todo
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

- [ ] Golden tests for the three modes with context and truncation; results
      are identical on Windows and Linux (CRLF files, path separators).
- [ ] Respects `.gitignore`/`.harness/ignore`; `glob` filter narrows.
- [ ] 100k-file tree search of a rare token completes < 1 s on the reference
      machine.
- [ ] `multiline: true` matches across lines; default does not.

## Verification

`cargo test -p apprentice-core tools::grep::` with a fixture tree.

## Notes

- Do not depend on a `rg` binary being installed; the crates give the same
  semantics.
