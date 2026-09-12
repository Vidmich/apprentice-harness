# M01-10 — Session persistence, resume, search

Status: todo
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

- [ ] Kill the daemon mid-session (after tool results, before next call);
      restart; `harness run --session ID "continue"` succeeds against the
      mock with a history that passes the invariant checks.
- [ ] Crash with unresolved `tool_use` → repaired on load and logged.
- [ ] FTS search finds a word from an assistant message; archived sessions
      hidden by default.
- [ ] Auto-title generated via the cheap model and recorded in
      `mentor_calls` with kind `title`; disabled by config.
- [ ] `session.export` JSON re-imports (round-trip test) into a fresh store.

## Verification

Integration tests in `crates/daemon/tests/session_resume.rs`.

## Notes

- `content_json` stores what the API saw (post-truncation tool results);
  the raw outputs are in trace blobs. Resume must never inflate history
  with raw outputs.
