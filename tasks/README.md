# Tasks

Task files for apprentice-harness, grouped by milestone (see `../ROADMAP.md`).
Tasks for a milestone are written when that milestone starts; M00 and M01 are
filled because M01 (a usable, trace-capturing assistant) is the first priority.

## Layout

```
tasks/
  README.md              this file
  M00-foundations/       M00-01-*.md ... one file per task
  M01-usable-assistant/
  M02-local-inference/   README.md placeholder until the milestone starts
  ...
```

Task id = `M<milestone two digits>-<task two digits>`; file name =
`<id>-<kebab-slug>.md`. Ids are stable; never renumber. Insert late tasks
with the next free number.

## Task file format

Every task is self-contained: an implementer with only `SPEC.md`, this task
file and the repository must be able to finish it. Sections, in order:

```
# <id> — <title>

Status: todo | in-progress | done | blocked
Depends on: <ids or "none">
Size: S (≤ half a day) | M (1–2 days) | L (3–5 days)

## Goal            one paragraph: what exists when this is done
## Context         why; the SPEC sections it implements; decisions already made
## Scope           in / out bullets
## Design          concrete design: modules, types, schemas, commands, file paths
## Acceptance      checklist the reviewer ticks
## Verification    how to prove it: commands to run, tests to write
## Notes           risks, open points, references
```

## Fixed decisions (apply to all tasks)

These were decided in SPEC.md and during planning; tasks assume them.

- **Repository layout**

  ```
  Cargo.toml                 Rust workspace
  crates/core/               lib  apprentice_core   — runtime, tools, mentor, traces, inference, eval
  crates/api/                lib  apprentice_api    — JSON-RPC method/event types shared by daemon and clients
  crates/common/             lib  apprentice_common — paths + logging setup shared by every process (no core)
  crates/client/             lib  apprentice_client — daemon discovery/spawn + typed RPC client (CLI, GUI)
  crates/daemon/             bin  harnessd          — hosts core, serves the RPC API
  crates/cli/                bin  harness           — CLI
  apps/gui/                  Tauri 2 + React + Vite + TypeScript (pnpm)
  ml/                        Python (uv, 3.12), package apprentice_ml
  protocol/                  mentor–apprentice prompt protocol versions (from M03)
  tasks/  SPEC.md  ROADMAP.md
  ```

- **Rust**: stable toolchain, edition 2024, `tokio` (multi-thread), `serde`/`serde_json`,
  `reqwest` (rustls, streaming), `rusqlite` (bundled), `clap` 4 (derive),
  `tracing`, `directories`, `keyring`, `thiserror` (libs) / `anyhow` (bins),
  `uuid` (v7), `time`, `sha2`, `interprocess` (local sockets). No `unsafe`
  outside the llama.cpp binding crate (M02).
- **Ids**: UUID v7 strings for sessions, agents, steps, events, calls.
  Timestamps: RFC 3339 UTC with microseconds.
- **Data directories** (`directories::ProjectDirs::from("", "", "apprentice-harness")`):
  config dir holds `config.toml`; data dir holds `traces.sqlite`, `blobs/`,
  `logs/`, `models/`, `daemon.json`, `daemon.lock`. Override with
  `HARNESS_HOME=<dir>` (both config and data under it) — used by tests.
- **IPC**: JSON-RPC 2.0, newline-delimited JSON, over a local socket
  (Unix domain socket / Windows named pipe). Server→client streaming uses
  JSON-RPC notifications. One daemon per user; clients spawn it if absent.
- **Mentor**: Anthropic Messages API over raw HTTPS (no Rust SDK exists).
  Default model `claude-opus-5`, adaptive thinking, effort `high`, always
  streaming, prompt-cache-friendly prefix layout, full `usage` captured.
  Model id, effort and `max_tokens` are config, never constants.
- **Traces**: append-only SQLite + content-addressed blob directory. No code
  path calls the mentor or a tool without writing to the trace store.
- **Errors**: typed `thiserror` enums in libs; every RPC error has a stable
  numeric code and a machine-readable `data.kind` string.
- **Tests**: unit tests beside code; integration tests under `crates/*/tests`
  using `HARNESS_HOME` pointing to a temp dir and a mock mentor HTTP server
  (`wiremock`) — never the real API in CI.
- **Naming**: the remote model is the *mentor*, the local model is the
  *apprentice*, everywhere in code, config and UI.
