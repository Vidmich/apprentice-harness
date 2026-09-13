# M01-10 — Session persistence, resume, search

Status: done
Depends on: M00-06, M01-08
Size: M

## Goal

Sessions survive daemon restarts: the conversation (all messages with
content blocks, including thinking signatures and tool results as sent to
the mentor) is persisted, can be resumed exactly, listed with titles and
activity, searched by text, archived, exported. The GUI sidebar and CLI
`session` commands are built on this.

## Context

SPEC §13 (session list, resume, search) and §9 (replayability). The trace
store already has every event; this task adds a **materialised
conversation** so resume does not need to reconstruct history from events
(fast, and robust to future event-format changes), while the trace stays
the source of truth.

## Scope

In: `session_messages` table, write-through from the runtime, load on
resume, title generation, list/search/archive/delete RPCs, export of a
session as JSON, CLI commands.
Out: trace bundle export (M01-14), GUI (M01-12).

## Design

### Storage (migration v003)

```sql
CREATE TABLE session_messages (
  session_id TEXT NOT NULL REFERENCES sessions(id), seq INTEGER NOT NULL,
  role TEXT NOT NULL, content_json TEXT NOT NULL,            -- exact ContentBlock array as sent/received (tool_result content already truncated for the mentor, i.e. what the API saw)
  agent_id TEXT, step_id TEXT, created_at TEXT NOT NULL,
  PRIMARY KEY(session_id, seq));
CREATE VIRTUAL TABLE session_fts USING fts5(session_id UNINDEXED, seq UNINDEXED, text);   -- text parts only
ALTER TABLE sessions ADD COLUMN message_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE sessions ADD COLUMN last_agent_status TEXT;
ALTER TABLE sessions ADD COLUMN prompt_version TEXT;
ALTER TABLE sessions ADD COLUMN tools_hash TEXT;
```

The runtime writes each message as it is appended to the in-memory
`Conversation` (same transaction as the corresponding trace event).

### Resume

`Conversation::load(session_id)` reads `session_messages` in order and the
session's `config_json` (model, effort, prompt_version, tools_hash). If the
current tool set or prompt version differs from the stored one, the
session continues with the *current* set (cache miss once; logged as
`session.prefix_changed`), because the API must see a consistent tool list
with the history's `tool_use` names — names that disappeared are handled
by the API as unknown-tool history (allowed) but the mentor is told in a
user-turn note: `[note: tool set changed since this session started]`.

Invariant checks on load: history alternates roles, every `tool_use` has a
matching `tool_result` in the next user message; if the last assistant
message has unresolved `tool_use` (crash mid-tools), drop that assistant
message and log `session.repaired`.

### Titles

First `agent.run` in a session sets the title to the first 60 chars of
the prompt; after the first `end_turn`, an async cheap call
(`mentor.title_model`, default `claude-haiku-4-5`, `max_tokens 30`, no
thinking) generates a ≤ 8-word title unless the user set one. The title
call is recorded like any mentor call (kind `title`) and counted in stats.
Disable via `sessions.auto_title = false`.

### RPC and CLI

`session.list {query?, workspace_id?, include_archived?, limit, offset}` →
summaries with `message_count, last_activity, last_agent_status, tokens,
cost`; `session.get {id}` → messages (paged); `session.search {query}` →
FTS hits with snippets; `session.archive`, `session.delete` (deletes
messages, keeps traces unless `purge_traces: true`), `session.rename`,
`session.export {id}` → JSON file `{session, messages, mentor_calls}`.

CLI: `harness session list|show ID|search Q|rename ID T|archive ID|delete ID|export ID -o file`.
`harness run --session ID` resumes; `harness run --last` resumes the most
recent session in the current workspace.

## Acceptance

- [x] Kill the daemon mid-session (after tool results, before next call);
      restart; `harness run --session ID "continue"` succeeds against the
      mock with a history that passes the invariant checks. —
      `daemon/tests/session_resume.rs` (`daemon stop` under the third
      call; the resumed request carries the five stored messages with
      `continue` joined to the last user turn, the same tools) and
      `core/tests/sessions.rs::every_message_is_stored_as_sent_and_the_
      session_resumes_after_a_restart`.
- [x] Crash with unresolved `tool_use` → repaired on load and logged. —
      `a_crash_between_a_call_and_its_tools_is_repaired_on_load`
      (`session.repaired {dropped_seq, tool_use_ids}`, the row gone; a
      history no repair fixes is refused).
- [x] FTS search finds a word from an assistant message; archived sessions
      hidden by default. — `search_finds_words_and_the_lifecycle_methods_
      hide_rename_and_delete`.
- [x] Auto-title generated via the cheap model and recorded in
      `mentor_calls` with kind `title`; disabled by config. —
      `the_cheap_model_titles_a_session_after_its_first_answer`,
      `auto_title_off_leaves_the_prompt_title`.
- [x] `session.export` JSON re-imports (round-trip test) into a fresh store.
      — `an_export_round_trips_into_a_fresh_store`.

## Verification

Integration tests in `crates/daemon/tests/session_resume.rs`.

## Notes

- `content_json` stores what the API saw (post-truncation tool results);
  the raw outputs are in trace blobs. Resume must never inflate history
  with raw outputs.

## Completion notes (2026-09-12)

`cargo test -p apprentice-core --test sessions` and `-p harnessd --test
session_resume`:

- Schema v3 (`trace/migrations/v003.sql`): `session_messages` (one row
  per message, `content_json` the exact block array, `seq` = position
  from 1), `session_fts` (fts5 over the text blocks), the `sessions`
  columns `message_count`, `last_agent_status`, `prompt_version`,
  `tools_hash` and `title_source` (`user|prompt|generated`), and
  `mentor_calls.kind` (`step|title`). `trace/sessions.rs` holds the
  store side: `put_session_message` (upsert by `(session, seq)`, so a
  user turn that grows rewrites its row), `append_with_message` (event
  + row in one transaction), `session_messages`, `truncate_session_
  messages`, `list_sessions(SessionQuery)` with activity, token totals
  and cost per row, `search_sessions` (every word a quoted prefix, all
  required; snippets with `[match]`), `archive_session`,
  `delete_session(purge)`, `export_session` / `import_session`.
- Write-through (`runtime`): the user turn is pushed and stored in
  `run_agent_with` in the same transaction as `user.message`; the
  assistant turn with `assistant.message` (`after_response`); the tool
  results after the step; the `[continue]` turn with its event. The
  in-memory conversation and the rows are the same list at all times
  (`Conversation::last_row`), which is what makes the repair a
  `DELETE ... WHERE seq > keep`.
- Resume (`runtime::load_conversation`, `AppState::conversation`): the
  first run after a restart loads the rows, seeds the tool hash from
  the session so `set_tools` reports a change, drops a trailing
  assistant turn with unanswered tool calls (in memory and in the
  store, `session.repaired`), refuses anything else `validate` rejects,
  and seeds the running totals from `mentor_calls`. The end of a run
  repairs the same way (a trace failure between a call and its tools).
- Prefix (`agent::record_prefix`): the first run writes
  `prompt_version` and `tools_hash` to the session; a later run whose
  values differ records `session.prefix_changed {prompt_version?,
  tools_hash?}`, updates the columns, warns (`tools_changed`), and —
  when the tools differ — adds `[note: tool set changed since this
  session started]` to the user turn (stored too).
- Titles (`sessions::title`): an untitled session gets the first line
  of its first prompt (60 chars) at the first run; after a run that
  ended `ok` with an answer, `maybe_generate` spawns (under the agent
  registry, so shutdown waits) one call to `mentor.title_model`
  (`claude-haiku-4-5-20251001`, `max_tokens 30`, thinking disabled,
  effort low) on a fresh step of the finished agent, recorded like any
  call (kind `title`, priced, in the stats), then `session.title
  {title, source, call_id}`. `set_session_title` never lets a
  generated title replace the user's. Config: `[sessions] auto_title`,
  `mentor.title_model`.
- RPC (`sessions` module, all `session.*` incl. `create`):
  `list {query?, workspace?, workspace_id?, include_archived, limit,
  offset}` (title substring or FTS match; open only by default),
  `get {id, after_seq?, limit?}` → info + a page of messages,
  `search {query, include_archived, limit?}`, `archive {id, archived}`,
  `delete {id, purge_traces}` (without purge: rows gone, the session
  row stays `deleted` with its traces; with: events, calls, steps,
  agents and the row go, blob refcounts released), `rename {id,
  title}`, `export {id}` → `SessionExport {format:
  "harness-session/1", session, messages, mentor_calls}`. The
  mutations are `conflict` while an agent runs on the session; a run
  on a deleted session is too. `api.ts` mirrors it (34 methods).
- CLI: `harness session list [--query] [--workspace] [--all] | show ID
  [--after] [--limit] [--full] | search Q [--all] | rename ID T |
  archive ID [--undo] | delete ID [--purge-traces] | export ID [-o F]`;
  `harness run --last [--workspace DIR]` resumes the workspace's most
  recent session.

Deviations / decisions:

- The `user.message` event and the row of the user turn are one
  transaction, as designed, but the turn is pushed before `Run::setup`
  (was: after), so a run that fails at setup (no key) still leaves the
  prompt in the conversation and the store; the next run's prompt joins
  it. The alternative (rows that can disagree with memory) was worse.
- The provisional prompt title writes no `session.title` event: it is
  a placeholder, and the exact event order of the run tests stays.
- On resume the `#workspace` block is rebuilt by the new daemon (fresh
  git state), a cache miss on that block only; the core block is the
  same bytes. `session.prefix_changed` fires on the prompt *version*,
  not on the context text.
- `mentor.title_model` defaults to the dated id
  `claude-haiku-4-5-20251001` (the pricing table's key), not the alias.
- `session.delete` without `purge_traces` keeps the row as status
  `deleted` (a third `SessionStatus`), since its traces still point at
  it; `include_archived` lists those too.
- Exports carry no request bodies; `import_session` gives each call a
  minimal `mentor.request` event (`imported: true`) so the row's
  foreign key holds. Trace bundles are M01-14.
- The existing test harnesses set `sessions.auto_title = false`: the
  title call would otherwise take the next scripted response and land
  in the trace after `agent.finished` at an unpredictable time.
