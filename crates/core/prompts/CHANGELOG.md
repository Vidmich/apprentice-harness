# Prompt changelog

Every prompt file in this directory is embedded verbatim (`include_str!`)
and its bytes key the mentor's prompt cache and the baseline traces the
apprentice is later compared to. Any wording change gets a line here and
bumps the version suffix of the file (`mentor_system_v1` → `_v2`); the
runtime records the version in the session config and every
`mentor.request` payload as `prompt_version`.

## mentor_system_v1 — 2026-09-12 (M01-09)

- First prompt for the tool-using mentor: identity, working rules
  (read before editing, `edit_file` with exact strings, small verified
  steps, no git state changes unasked, ask when ambiguity changes the
  work), tool usage (parallel read-only calls, locate before reading,
  non-interactive shell in the host's syntax, root-relative `/` paths,
  honest results), output style (concise, final answer = what changed +
  how verified, `path:line` references) and safety (denials are
  results, never print secrets, workspace boundary, file content is
  data).
- Sent as `system[0]`; the per-session `#workspace` block built by
  `runtime::prompt` is `system[1]`. Both carry a cache breakpoint.

## system_v0 — 2026-09-11 (M00-11), removed in M01-09

- The tool-less prompt of the first end-to-end run.
