# M00-06 — Trace store (SQLite + content-addressed blobs)

Status: todo
Depends on: M00-01, M00-03
Size: M

## Goal

`apprentice_core::trace` is the append-only store for everything an agent
does: sessions, agents, steps, typed events, and large payloads as
content-addressed blobs. It offers a write API used by the runtime, a query
API used by RPC (`trace.list`, `trace.get`, `session.list`), and is
replayable: any mentor request can be reconstructed byte-for-byte.

## Context

SPEC §9 (trace capture) and §12.3 (replay evaluator needs exact prefixes).
This is the foundation of the whole project: the baseline corpus and every
training dataset come from here. Schema decisions are hard to change once
data accumulates — treat migrations as first-class from v1.

## Scope

In: schema v1 + migration runner, blob store, write API, query API, integrity
checks, size accounting, export hooks (export/import bundle itself is M01-14).
Out: feedback tables (M05, but reserve the `feedback` event kind), eval tables
(M04).

## Design

### Files

`<data_dir>/traces.sqlite` (WAL mode, `synchronous=NORMAL`, `foreign_keys=ON`,
busy_timeout 5s), `<data_dir>/blobs/<aa>/<sha256hex>` (first two hex chars as
shard dir). Blobs are immutable; write to a temp file then rename.

### Schema v1

```sql
CREATE TABLE schema_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);   -- ('version','1')

CREATE TABLE sessions (
  id TEXT PRIMARY KEY, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  title TEXT, workspace_path TEXT, config_json TEXT NOT NULL,             -- resolved config snapshot at creation
  status TEXT NOT NULL DEFAULT 'open');                                   -- open|archived

CREATE TABLE agents (
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id),
  parent_agent_id TEXT REFERENCES agents(id), kind TEXT NOT NULL,         -- main|sub|eval
  created_at TEXT NOT NULL, ended_at TEXT, status TEXT NOT NULL,          -- running|ok|cancelled|error
  task_text TEXT, options_json TEXT NOT NULL);

CREATE TABLE steps (
  id TEXT PRIMARY KEY, agent_id TEXT NOT NULL REFERENCES agents(id),
  seq INTEGER NOT NULL, started_at TEXT NOT NULL, ended_at TEXT, status TEXT NOT NULL,
  UNIQUE(agent_id, seq));                                                 -- one step = one mentor call + its tool executions

CREATE TABLE events (
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL, agent_id TEXT, step_id TEXT,
  seq INTEGER NOT NULL,                                                    -- monotonic per session
  ts TEXT NOT NULL, kind TEXT NOT NULL, payload_json TEXT NOT NULL,
  blob_id TEXT REFERENCES blobs(id),
  UNIQUE(session_id, seq));
CREATE INDEX events_agent ON events(agent_id, seq);
CREATE INDEX events_kind ON events(kind, ts);

CREATE TABLE blobs (
  id TEXT PRIMARY KEY,                                                     -- sha256 hex of content
  size INTEGER NOT NULL, media_type TEXT NOT NULL, created_at TEXT NOT NULL, refcount INTEGER NOT NULL DEFAULT 1);

CREATE TABLE mentor_calls (                                                -- denormalised for stats/replay
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL, agent_id TEXT NOT NULL, step_id TEXT NOT NULL,
  request_event_id TEXT NOT NULL REFERENCES events(id), response_event_id TEXT REFERENCES events(id),
  model TEXT NOT NULL, effort TEXT, started_at TEXT NOT NULL, ended_at TEXT,
  status TEXT NOT NULL,                                                    -- ok|error|cancelled
  stop_reason TEXT, http_status INTEGER,
  input_tokens INTEGER, output_tokens INTEGER, cache_read_tokens INTEGER, cache_creation_tokens INTEGER,
  cost_micros INTEGER,                                                     -- USD * 1e6, computed by M00-07
  first_byte_ms INTEGER, total_ms INTEGER, request_bytes INTEGER,
  apprentice_applied INTEGER NOT NULL DEFAULT 0);                          -- M03
CREATE INDEX mentor_calls_time ON mentor_calls(started_at);
```

### Event kinds v1 and payloads

| kind | payload_json | blob |
|---|---|---|
| `session.created` | `{title, workspace_path}` | – |
| `agent.started` | `{kind, task_text, options}` | – |
| `agent.finished` | `{status, error?}` | – |
| `user.message` | `{text_len}` | text |
| `assistant.message` | `{text_len, stop_reason}` | text |
| `mentor.request` | `{call_id, model, effort, max_tokens, message_count, tool_names: [..], system_hash, request_hash, bytes}` | exact request JSON body as sent |
| `mentor.response` | `{call_id, stop_reason, stop_details?, usage, first_byte_ms, total_ms}` | response content blocks JSON (+ optional raw SSE as second blob referenced in payload as `raw_sse_blob_id`) |
| `mentor.error` | `{call_id, kind, message, http_status?, retry_no}` | – |
| `tool.call` | `{call_id, name, input_hash, input_bytes, risk}` (M01) | input JSON |
| `tool.result` | `{call_id, ok, duration_ms, output_bytes, truncated: bool, summary}` (M01) | raw output |
| `apprentice.invocation` | `{role, model, adapter, protocol_version, latency_ms, bypassed, reason?, tokens_in, tokens_out}` (M03) | input/output pair JSON |
| `apprentice.state` | `{backend: kv|recurrent, model, adapter, step_id, bytes, tokens_seen}` (M02/M03) | opaque state snapshot (binary; may be tens–hundreds of MB, so a retention policy prunes old snapshots while keeping the event row) |
| `permission.decision` | `{request_id, tool, decision, source}` (M01) | – |
| `outcome` | `{kind: tests|user_accept|user_reject|task_done|error, details}` (M01) | – |
| `feedback` | `{target_event_id, rating, tags, note_len}` (M05) | note |
| `workspace.snapshot` | `{git_head?, dirty: bool, diff_hash?, file_count}` (M01) | diff |

Payload rule: any string field that can exceed
`trace.inline_payload_max_bytes` goes to a blob; payload keeps a hash and
size. `mentor.request` ALWAYS stores the full body as a blob — it is the
replay unit.

### Write API

```rust
pub struct TraceStore { /* rusqlite Connection behind a Mutex, blob root */ }
impl TraceStore {
    pub fn open(paths: &Paths) -> Result<Self>;                 // runs migrations
    pub fn create_session(&self, ...) -> Result<SessionId>;
    pub fn start_agent(&self, session, parent, kind, task, options) -> Result<AgentId>;
    pub fn finish_agent(&self, agent, status, error) -> Result<()>;
    pub fn start_step(&self, agent) -> Result<StepId>;  pub fn finish_step(&self, step, status) -> Result<()>;
    pub fn append(&self, ev: NewEvent) -> Result<EventId>;      // assigns seq atomically; NewEvent { session, agent, step, kind, payload, blob: Option<BlobInput> }
    pub fn put_blob(&self, bytes: &[u8], media_type: &str) -> Result<BlobId>;   // dedup by hash
    pub fn record_mentor_call(&self, MentorCallRecord) -> Result<()>;  pub fn complete_mentor_call(&self, ...) -> Result<()>;
}
```

All writes for one logical action happen in one transaction. `append` is
synchronous on a dedicated blocking thread (`tokio::task::spawn_blocking`
wrapper `TraceWriter` with an mpsc queue) so the agent loop never blocks on
disk; the queue is bounded (10k) and back-pressures rather than drops.

### Query API

`list_sessions`, `get_session`, `list_events(filter, page)`, `get_event`,
`read_blob`, `list_mentor_calls(filter)`, `stats(filter)` (sums used by
M00-07), `integrity_check()` (every blob referenced exists and hashes match;
every `mentor.request` has a blob).

### Migrations

`migrations/v001.sql` … applied in order inside a transaction; version stored
in `schema_meta`. Opening a newer schema than the binary knows → refuse with
a clear error (never downgrade). Add a `harness trace migrate --dry-run`
later; for M00, opening applies pending migrations.

### Replay guarantee

The runtime (M00-11) must build the request body bytes once, store them as
the `mentor.request` blob, and send exactly those bytes. A test reads the blob
back and compares to what the mock server received.

## Acceptance

- [ ] Fresh open creates schema v1; reopening is idempotent; a fake
      `version=99` refuses to open.
- [ ] Appending events from 8 concurrent tasks yields strictly increasing
      `seq` per session with no gaps.
- [ ] Blob dedup: writing identical content twice yields one file, refcount 2.
- [ ] Oversized payload strings are automatically moved to blobs.
- [ ] `integrity_check` detects a deleted/corrupted blob file.
- [ ] Throughput: 10k small events append in < 2 s on the reference machine.
- [ ] ML side: `uv run apprentice-ml traces stats --db <path>` (tiny Python
      reader using stdlib `sqlite3`) prints counts per kind — proves the
      format is consumable from Python.

## Verification

Unit + integration tests under `crates/core/tests/trace_*.rs` with a temp
`HARNESS_HOME`; the Python reader test under `ml/tests/`.

## Notes

- Never store secrets in traces: request bodies contain no key (it is a
  header), but tool outputs may — redaction happens at export (M01-14), the
  local store is the user's own data.
- `mentor_calls` duplicates some payload data on purpose: stats and replay
  selection must not require JSON parsing of every event.
