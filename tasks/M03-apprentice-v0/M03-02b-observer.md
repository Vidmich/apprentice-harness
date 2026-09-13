# M03-02b — Observer

Status: todo
Depends on: M02-04b, M03-02
Size: M

## Goal

The primary role backend: one resident `SessionState` (M02-04b) per
session, fed with every session event as it is appended to the trace —
user text and mentor output verbatim, tool results as their compressed
form plus reference — so the roles become *queries against that state*
("compress this result given what you know", "what does the mentor need
now", "emit `#state`") instead of prompts re-assembled from the
transcript. The state is snapshotted into the trace at every step, forked
when a sub-agent is spawned, and the session is demoted to the stateless
backend — with a trace record — whenever the state is evicted, errors, or
runs out of context.

## Context

SPEC §4 ("Primary shape: the observer … The hooks above are then queries
against that state"), §5.0 (input: the event stream; answers `#state`
from state; "state is snapshotted into the trace at every step … so any
step can be replayed exactly and a sub-agent can be forked from the
parent's state"; fallback: stateless roles). M02-04b's `SessionState
{append, generate(commit), snapshot, restore, fork, drop_state}` and
`StateManager {open, get, evict}` with `StateError::{Demoted,
ContextFull}`; M03-02's `RoleBackend { query, ingest }` and `backend =
"auto"`. The README decision: mentor text and user messages verbatim,
tool results as the compressed form plus reference.

## Scope

In: the observer backend, the ingestion format, the query prompts'
wiring (`protocol/v1/roles/observer.md`), per-step snapshots, fork on
spawn, self-compaction when the state is full, demotion and recovery,
the `apprentice.state` records, metrics, tests on the mock backend.
Out: the roles' own logic (they call `query`), M07's multi-agent
spawning (the fork API is exercised by a test and by `harness apprentice
state fork` only), training on the snapshots (M06).

## Design

### Ingestion (`runtime/apprentice/observer.rs`)

`ObserverBackend { state: Arc<dyn SessionState>, ingested_seq: u64,
prefix_tokens: u32 }`. On `open` the state is seeded with the observer
prompt (`roles/observer.md` "ingest" section: who it is, the tag format
of what follows) — the same bytes for every session, so the service can
`seq_cp` it (M02-04's shared prefix) instead of prefilling it. Then every
trace event of the session with `seq > ingested_seq` is appended in
order, rendered as compact tagged lines:

```
@user <text verbatim>
@mentor <text blocks verbatim; tool_use as `call <name> <compact json ≤ 200 chars>`; thinking omitted>
@result <tool_call_id> <the #result block the compressor produced, or the M01-01 truncated text when no compressor ran, or `ref blob:<id> bytes=<n>` alone for results above observer.max_result_bytes>
@state v=<n> <the last #state the compactor produced, when the observer did not author it>
@step <n>
```

Appends are `Priority::Background` and asynchronous: the orchestrator
calls `ingest` after each trace write; a query first drains pending
appends (`ensure_ingested(seq)`), so a role always sees the whole
history up to its step. `tokens_seen` per event is recorded in
`apprentice.state {event: "ingest", seq, tokens}` at `debug` granularity
(one row per step, summed).

### Queries

`query(role, q)` → `state.generate(query_prompt(role, q), params,
commit = false)`: the query and the answer are rolled back afterwards
(the state only ever holds the session's events), except `#state`
answers, which are committed as `@state` lines so the observer's own
summaries are part of what it has seen (they replace nothing). Query
prompts live in `observer.md` and are short because the input is already
in the state: `compress <tool_call_id>` (M03-03 passes the raw result's
locators, not its text — the state has the text by reference; the
compressor query includes the raw text once, uncommitted), `select
<task>` (M03-06), `state` (M03-07), `ask <text>` (M03-02).

### Snapshots and forks

After every step's tools ran and the appends drained: `state.snapshot()`
→ `apprentice.state {event: "snapshot", step_id, tokens_seen, bytes,
took_ms, backend: kv|recurrent}` with the blob (M02-04b's retention
prunes old blobs, rows stay). `observer.snapshot_every = 1` step by
default; KV backends with snapshots over `observer.snapshot_max_ms` (200)
switch to every N steps with a warning once (SPEC §5.0 "may be sampled
instead"). `fork(for_session)` is exposed as `Orchestrator::fork_state
(parent, child)` for M07 and `harness apprentice state fork` for testing.

### Context full → self-compaction

`append` returning `ContextFull`: the observer queries `state` (commit
= false), drops the state (`drop_state`), opens a fresh one, seeds the
prompt, appends `@state v=<n+1>` with the summary and `@step`, then
continues ingesting. Recorded as `apprentice.state {event: "compacted",
tokens_before, tokens_after}` and as an `apprentice.invocation {role:
"compactor", backend: "observer", trigger: "state_full"}`. The mentor's
conversation is untouched by this — M03-07 decides separately when the
*mentor's* history is compacted.

### Demotion and recovery

Any `StateError::Demoted`, `Unavailable`, a query exceeding its budget
twice in a row, or `open` failing → the session's backend becomes
`Stateless` (M03-02 reads `backend_for(session)`), recorded once as
`apprentice.state {event: "demoted", reason}`; every following
invocation carries `backend: "stateless", demoted_from: "observer"`.
Recovery: at the next step boundary the orchestrator tries `open`
again (M02-04b restores the latest snapshot when one exists, else the
observer re-ingests from the trace — bounded by `observer.reingest_max_
tokens`, 32k; beyond that a fresh state with the last `#state`);
success → `{event: "restored", from: snapshot|reingest}` and back to
the observer. `observer.demote_cooldown_s = 60` avoids flapping.

### Config and surfaces

```toml
[apprentice.observer]
enabled = true            # used when backend = observer|auto
max_result_bytes = 8192   # larger results are ingested by reference only
snapshot_every = 1
snapshot_max_ms = 200
reingest_max_tokens = 32768
demote_cooldown_s = 60
```

RPC `apprentice.state {session_id}` → `{backend, tokens_seen,
bytes_resident, snapshots, demoted, last_query_ms}`; `harness apprentice
state show|snapshot|fork|evict <session>`; `inference.status` (M02-05)
gains `observers: n`.

## Acceptance

- [ ] Mock inference backend (M02-04's `MockBackend` with the M02-04b
      state ops): a 6-step scripted session ingests every event in
      order (the mock records the appended text; a golden compares it
      to the expected `@user/@mentor/@result/@step` rendering, thinking
      absent, a 20 KB result rendered by reference).
- [ ] A query mid-session drains pending appends first; `commit = false`
      leaves `tokens_seen` unchanged; a `state` query commits its
      `@state` line.
- [ ] One `apprentice.state {event: "snapshot"}` per step with a blob;
      with `snapshot_max_ms` exceeded (mock delay) the cadence drops and
      one warning event is written.
- [ ] `ContextFull` from the mock at step 4 → self-compaction: a fresh
      state seeded with the summary, `compacted` record, ingestion
      continues, the next query works.
- [ ] Demotion: the mock returns `Demoted` → the next invocation is
      `backend: stateless, demoted_from: observer`; after the cooldown
      `open` succeeds from the snapshot → `restored` and `backend:
      observer` again; with no snapshot → reingest from the trace and
      the mock's appended text equals the golden.
- [ ] `fork` produces a child state whose `tokens_seen` equals the
      parent's and whose next append goes to the child only; `harness
      apprentice state fork` does it on a live daemon.
- [ ] Live on the reference machine (3B model, KV backend): a 15-step
      dogfood session keeps the observer resident, snapshots cost what
      M02-07's cost table predicted (± 30 %), and `harness stats
      apprentice --by backend` shows ≥ 90 % of invocations on the
      observer.

## Verification

`cargo test -p apprentice-core runtime::apprentice::observer::`; the live
session with the `apprentice.state` rows and timings in the completion
notes.

## Notes

- The observer never sees raw blobs larger than `max_result_bytes`; the
  compressor query passes the raw text uncommitted, so the state stays
  small and the exact data stays in the trace (SPEC §4: "Exact data is
  never expected to be in the state").
- Whether the observer beats the stateless backend is M04's question
  (replay evaluator on both backends, same steps); this task only has
  to make both produce the same block types so the comparison is fair.
