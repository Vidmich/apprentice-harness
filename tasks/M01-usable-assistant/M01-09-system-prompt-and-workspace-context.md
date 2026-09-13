# M01-09 — System prompt v1 and workspace context

Status: done
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

- [x] Assembled system for a fixture workspace matches a snapshot; token
      count reported. — `core/tests/prompt.rs::the_workspace_block_of_a_
      fixture_matches_the_snapshot` (`snapshots/prompt__workspace_block.
      snap`); `harness prompt show --count` reports `count_tokens` (or
      why it could not: `prompt_show_assembles_for_a_workspace_and_
      answers_for_a_session` without a key).
- [x] `HARNESS.md` present → included; > 8 KiB → truncated with note;
      absent → section omitted (byte-identical core still first). —
      `instructions_are_included_truncated_or_omitted`.
- [x] The core block bytes are identical across two sessions on different
      workspaces (cache sharing). — the same test (fixture with and
      without instructions, and no workspace at all) and
      `daemon/tests/e2e_loop.rs` (`prompt show --session` vs `--workspace`).
- [x] `prompt_version` appears in `mentor.request` payloads and session
      config. — `core/tests/loop.rs::a_three_step_trajectory_...` (every
      `mentor.request` payload, `sessions.config_json`),
      `app::tests::sessions_are_created_with_a_config_snapshot`,
      `trace_store.rs`.
- [ ] Live check: second call of a session shows
      `cache_read_input_tokens > 0` in usage (prefix caching works). —
      needs an API key on the machine; not run yet. Against the mock the
      request layout is asserted (`cache_control` on both system blocks,
      the last tool, the last user block).

## Verification

Unit tests for assembly; live check via `harness stats tokens` / trace
inspection after a two-turn session.

## Notes

- Wording changes after dogfooding begins should go through a
  changelog entry and, once M04 exists, a replay evaluation.

## Completion notes (2026-09-12)

`core::runtime::prompt` (`cargo test -p apprentice-core --test prompt`,
plus the prompt assertions in `--test loop` and the daemon's
`e2e_loop`):

- `crates/core/prompts/mentor_system_v1.md` (≈ 3.5 KB, ≈ 900 tokens by
  the 4-chars rule; `harness prompt show --count` gives the exact
  number) embedded as `MENTOR_SYSTEM_V1`; `prompts/CHANGELOG.md` opened
  with the v1 entry and the removal of `system_v0.md`. `PROMPT_VERSION
  = "mentor_system_v1"`.
- `WorkspaceContext::gather(workspace, Host)` → `render()`: the
  `#workspace` block of the design — `root` (`/`-separated), `os ·
  shell · harness` (`sysinfo` name + version, the shell the config's
  `tools.shell` resolves to, the crate version), `git` (one `git status
  --porcelain=v2` through the M01-06 `Repo`: `branch main @ 3f2a1c9,
  2 modified, 1 untracked` / `, clean` / `detached @` / `(no commits
  yet)` / `not a git repository` / `git is not installed`),
  `languages` (share of indexed files by `language_of`, ≥ 1%, six at
  most), `top-level` (the walker at depth 1 with the ignore rules,
  directories first, 40 at most, `… (+N more)`), and `#instructions
  (from .harness/HARNESS.md)` verbatim up to 8 KiB, cut at a character
  boundary with `[truncated: the first N of M bytes are shown; ...]`.
  A session without a workspace gets `root: none (...)` and the host
  line only. `build_system(workspace, &config)` assembles
  `SystemPrompt { version, blocks: [core, context] }`.
- `Conversation::set_system(SystemPrompt)` keeps the version;
  `request()` now puts a breakpoint on every system block (two), so the
  core is one cache entry for all sessions and the context another
  per session — with the last tool and the last user block that is the
  four the API allows. The runtime builds the prompt at the first run
  of a session (`Run::setup`, async now) and never again.
- Recorded: `session.create` writes `prompt_version` into the
  session's `config_json` (M01-10 reads it back);
  `TraceStore::record_mentor_request` takes the version and puts it in
  the `mentor.request` payload.
- RPC `prompt.show {session_id?, workspace?, count}` →
  `{version, blocks: [{text, cache}], session_id?, workspace?, tokens?,
  token_error?}`: a session's live blocks when this daemon has them
  (and no agent holds the conversation), else assembled for the
  session's workspace, or for `workspace`, or for none. `count` asks
  the mentor's `count_tokens` over the blocks plus a one-word user
  message; a failure (no key, network) is `token_error`, not an error.
  CLI `harness prompt show [--session ID | --workspace DIR] [--count]
  [--json]` prints the blocks with a header per block and the count.
  `api.ts` mirrors the method (28 methods).

Deviations / decisions:

- Language shares are by file count, not bytes (a lock file or a
  vendored JSON would otherwise dominate), and only over files with a
  known language, so they need not add up to 100.
- The shell is named without its version (`pwsh`, not `pwsh 7.4`):
  a version costs a subprocess per session for one token of value.
- Top-level entries include dotfiles the ignore rules let through
  (`.github/`, `.harness/`, `.gitignore`), which is what the mentor
  will see with `list_dir` too.
- The context block is built at the first run, not at `session.create`:
  a session created hours before its first prompt would otherwise
  carry a stale git line. Once built it is frozen for the session (in
  memory; M01-10 decides what a resumed session does).
