# M01-09 — System prompt v1 and workspace context

Status: todo
Depends on: M01-02, M01-08
Size: S

## Goal

The mentor's system prompt for coding work: a frozen core (identity, tool
usage rules, editing discipline, output style) plus a per-session workspace
context block (OS/shell, root, git state, project instructions from
`.harness/HARNESS.md`), laid out for prompt caching and versioned as a
file so changes are auditable and evaluable.

## Context

SPEC §6 (prompt engineering is a first-class artifact; stable prefix
layout), §3.1 (workspace layer). In M01 the mentor works alone; the
apprentice sections are added by the protocol in M03. Baseline traces
recorded under this prompt are what the apprentice is later compared to,
so keep it stable once dogfooding starts.

## Scope

In: prompt files, assembly, workspace context, project instructions
discovery, cache breakpoint placement, prompt version recorded in traces,
CLI `harness prompt show`.
Out: apprentice/mentor protocol (M03), per-user custom prompt editing
(later; a `.harness/HARNESS.md` covers most needs).

## Design

### Files

```
crates/core/prompts/mentor_system_v1.md      frozen core, embedded via include_str!
crates/core/prompts/CHANGELOG.md             every wording change gets a line and bumps the version suffix
```

Prompt version string `mentor_system_v1` recorded in `sessions.config_json`
and in every `mentor.request` payload (`prompt_version`).

### Assembly (`runtime::prompt::build_system(session)`)

```
system[0]  core text (cache: true)                                   — identical for everyone, all sessions
system[1]  workspace context (cache: true)                           — identical for the whole session
```

Workspace context block content (built once at session start, ≤ ~600
tokens):

```
#workspace
root: C:/src/foo (displayed POSIX-style)
os: windows 11 · shell: pwsh 7.4 · harness 0.1.0
git: branch main @ 3f2a1c9, 2 modified, 1 untracked         (or "not a git repository")
languages: rust 61%, ts 30%, md 9%                          (from the file index)
top-level: Cargo.toml, crates/, apps/, ml/, README.md
#instructions (from .harness/HARNESS.md, 8 KiB max; truncated with a note)
<file content verbatim>
```

Anything volatile (time, per-turn state) is NOT in `system`; if the
mentor needs the date it can run a command. M03 will use the
mid-conversation `role: "system"` message mechanism for per-turn
apprentice state.

### Core prompt content (v1) — headings, to be written in the file

1. Identity: senior engineer working inside apprentice-harness on the
   user's workspace; the user talks through a chat UI; tools available.
2. Working rules: read before editing; prefer `edit_file` with exact
   strings; small verifiable steps; run tests when they exist; never
   commit unless asked; ask when a request is ambiguous in a way that
   changes the work.
3. Tool usage: parallelise independent read-only calls; use `grep`/`glob`
   before reading many files; keep shell commands non-interactive; the
   shell is PowerShell on Windows / POSIX sh elsewhere (the context block
   says which).
4. Output style: concise; final answer summarises what changed and how it
   was verified; file references as `path:line`.
5. Safety: permissions may deny a call — explain and adapt; do not try to
   bypass; never print secrets found in files.

Target ≤ 1200 tokens. This is the *baseline* prompt; do not add
apprentice-related text here (M03 adds a separate section file so the
baseline remains available for A/B).

### CLI

`harness prompt show [--session ID] [--json]` prints the assembled system
blocks and their token count (via `count_tokens`) for inspection.

## Acceptance

- [ ] Assembled system for a fixture workspace matches a snapshot; token
      count reported.
- [ ] `HARNESS.md` present → included; > 8 KiB → truncated with note;
      absent → section omitted (byte-identical core still first).
- [ ] The core block bytes are identical across two sessions on different
      workspaces (cache sharing).
- [ ] `prompt_version` appears in `mentor.request` payloads and session
      config.
- [ ] Live check: second call of a session shows
      `cache_read_input_tokens > 0` in usage (prefix caching works).

## Verification

Unit tests for assembly; live check via `harness stats tokens` / trace
inspection after a two-turn session.

## Notes

- Wording changes after dogfooding begins should go through a
  changelog entry and, once M04 exists, a replay evaluation.
