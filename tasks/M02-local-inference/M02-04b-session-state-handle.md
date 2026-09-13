# M02-04b — Session state handle

Status: todo
Depends on: M02-04
Size: M

## Goal

An architecture-agnostic `SessionState` handle over the inference
service: one resident state per session for the observer (SPEC §5.0),
with `append`, `generate` (a query against the state, committed or rolled
back), `snapshot`, `restore`, `fork` and `drop`. A KV-cache backend
implements it first (a resident sequence in the service's context,
`state_seq_save/load` for snapshots, `seq_cp` for forks); a recurrent-
state backend implements the same trait when a recurrent/hybrid model is
loaded. Snapshots are stored as `apprentice.state` trace blobs with a
retention policy, memory is accounted per resident state, and an
eviction policy demotes a session to the stateless fallback with a trace
record.

## Context

SPEC §4 (the observer is a long-lived per-session process whose roles
are queries against its state), §5.0 (state snapshotted into the trace at
every step so any step can be replayed exactly and a sub-agent can be
forked from the parent's state), §7 ("Architecture-agnostic session
state … recurrent backends cannot rewind to an arbitrary prefix — only
to a snapshot — so the orchestrator treats snapshots, not prefixes, as
the unit of rollback"). M03-02b builds the observer on this; M04-04
replays from the snapshots; M00-06 reserved the `apprentice.state` event
kind.

## Scope

In: the trait and both backends, resident sequences in the scheduler,
query semantics, snapshots to the trace with retention, memory
accounting and eviction/demotion, RPC surface for inspection, the
snapshot cost table.
Out: what the observer feeds into the state and when it queries (M03-02b);
sub-agent spawning itself (M07 — `fork` is the primitive).

## Design

### Trait

```rust
#[async_trait]
pub trait SessionState: Send + Sync {
    fn id(&self) -> StateId;  fn session(&self) -> SessionId;  fn backend(&self) -> StateBackend /* Kv | Recurrent */;
    async fn append(&self, tokens: Prompt) -> Result<Appended { tokens_seen: u32, took_ms: u32 }>;
    async fn generate(&self, query: Prompt, params: SampleParams, commit: bool, cancel: CancellationToken) -> Result<Response>;   // commit=false: the query and the answer are rolled back afterwards; true: they become part of the state
    async fn snapshot(&self) -> Result<Snapshot { bytes: Bytes, tokens_seen: u32, took_ms: u32 }>;
    async fn restore(&self, bytes: &[u8]) -> Result<u32>;
    async fn fork(&self, for_session: SessionId) -> Result<Arc<dyn SessionState>>;
    async fn drop_state(&self);
    fn info(&self) -> StateInfo { tokens_seen, bytes_resident, last_used, snapshots: u32, demoted: bool };
}
pub struct StateManager { .. }                         // in the service: state.inference().states()
impl StateManager {
    pub async fn open(&self, session: SessionId, model: Option<ModelRef>) -> Result<Arc<dyn SessionState>>;   // resident if budget allows, else restores the latest snapshot if one exists, else fresh
    pub async fn get(&self, session: SessionId) -> Option<Arc<dyn SessionState>>;
    pub fn list(&self) -> Vec<StateInfo>;
    pub async fn evict(&self, session: SessionId, reason: EvictReason) -> Result<()>;
}
```

### KV backend

The scheduler (M02-04) keeps `inference.resident_states` sequences apart
from the transient slots. A resident sequence is never picked for a
transient request. Operations map onto the scheduler's command channel:

- `append`: prefill the tokens at `pos = tokens_seen` in chunks of
  `n_ubatch` (interleaved with the batching loop like any prefill, at
  `Priority::Background` unless the caller says otherwise); no logits
  wanted except the last (so a following `generate` can start at once).
- `generate(commit=false)`: remember `mark = tokens_seen`; prefill the
  query, generate into the same sequence (it is the slot for the
  duration), stream the answer; afterwards `seq_rm(seq, mark, None)` —
  the state is exactly what it was. `commit=true` skips the removal and
  advances `tokens_seen`. While a query runs, other queries on the same
  state queue (one at a time per state; the orchestrator serialises
  anyway).
- `snapshot`: `state_seq_save(seq)` on the scheduler thread (blocking;
  tens of ms per 100 MB — measured in the cost table); bytes go to the
  trace as an `apprentice.state` blob with payload `{backend: "kv",
  model, adapter, step_id, tokens_seen, bytes, took_ms, n_ctx, kv_type}`.
  Optional `zstd` level 1 for the blob (KV at f16 compresses poorly;
  measured, on by default only if it helps ≥ 20 %).
- `restore`: `state_seq_load` into a resident sequence; `tokens_seen`
  from the payload; a size that does not fit the sequence's context →
  `StateError::TooLarge`.
- `fork`: `seq_cp(parent, child)` into another resident sequence (or,
  when none is free, snapshot + restore later — recorded as
  `fork_deferred`); the child's `session` is the sub-agent's session.

Context per resident sequence: the plan's `per_slot_ctx`; a state that
reaches it returns `StateError::ContextFull{tokens_seen}` on `append` —
the observer (M03) then compacts (it re-ingests a `#state` summary into
a fresh state), not this layer.

### Recurrent backend

Same trait; differences: `append` and `generate` cost O(tokens) with a
constant-size state; `generate(commit=false)` cannot `seq_rm` a tail, so
the backend takes a `state_seq_save` *before* the query and `state_seq_
load` after (both in-memory, not traced) — the cost table says what that
costs per model; `snapshot` bytes are fixed-size (the state) and small;
`fork` is a state copy. `StateBackend::Recurrent` is chosen when
`ModelMeta.is_recurrent`; hybrids (linear attention + a few full
attention layers) are treated as recurrent for rollback purposes.

### Memory accounting and eviction

`StateManager` tracks `bytes_resident` per state (`state_seq_size` after
each append, or the fixed size) against `inference.resident_budget_bytes`
(default = the planner's KV bytes for `resident_states` sequences).
When `open` needs a sequence and none is free, or the budget is exceeded:
evict the least-recently-used state that is not mid-query — snapshot it
(if its last snapshot is older than its last append) and `seq_rm` the
whole sequence. The handle stays valid: the next `append`/`generate`
restores from the snapshot if a sequence is free, else the call returns
`StateError::Demoted` and the manager records `apprentice.state
{event: "demoted", reason}`; M03-02b falls back to the stateless backend
for that step. A session whose agent finished keeps its state resident
until eviction or `inference.state_idle_min` (default 30) passes.

### Retention

`trace.state_snapshots_keep` (default 3 per session) — older
`apprentice.state` blobs are pruned (the event row stays, `pruned_at` set,
as M00-06 designed); `keep_all` for eval corpora; M04 can pin snapshots
by step (`trace.pin_snapshot {event_id}`) so a replay corpus survives
pruning.

### Surfaces

- RPC `inference.states` (list with sizes, ages, snapshot counts),
  `inference.state_evict {session_id}`, `inference.state_snapshot
  {session_id}` (manual snapshot for tests/eval).
- CLI `harness apprentice states`, `harness apprentice state snapshot ID
  | evict ID`.

### Snapshot cost table (deliverable, `docs/state-costs.md`)

For each candidate model of M02-03 on the reference machine and on CPU:
bytes per state at 4k / 16k / 32k tokens seen (transformer: KV size ×
tokens; recurrent: fixed), snapshot and restore latency, `seq_cp` fork
latency, and the size of a zstd-compressed snapshot. This table decides
how often the observer snapshots (every step vs. sampled) — a decision
listed for M03-02b.

## Acceptance

- [ ] Against the mock backend: `append` then `generate(commit=false)`
      twice yields the same answer both times and `tokens_seen` is
      unchanged; `commit=true` advances it; a query while another runs on
      the same state waits rather than interleaves.
- [ ] `snapshot` → `drop_state` → `open` (restores) → `generate` gives the
      same answer as before the drop; the `apprentice.state` event and
      blob exist with the documented payload; a fourth snapshot prunes
      the first blob and keeps its row.
- [ ] `fork` produces a child whose first query equals the parent's, and
      appending to the child leaves the parent unchanged.
- [ ] Eviction under a 2-state budget with 3 sessions: the LRU one is
      snapshotted and evicted; its next call restores when a sequence is
      free and returns `Demoted` (with the trace record) when none is.
- [ ] Real backend, 3B model (`#[ignore]`, GPU): a 16k-token state
      snapshots and restores byte-exact (`state_seq_size` matches, the
      next greedy token matches); the cost table is filled for at least
      the 3B transformer and, if M02-01 got one loading, the recurrent
      candidate.
- [ ] A resident state never steals a transient slot: the M02-04 load
      test passes unchanged with two resident states open.

## Verification

`cargo test -p apprentice-core inference::state::` (mock); the GPU test
in the nightly job; the cost table produced by `harness models bench
--states` (M02-07 adds the flag; until then a `#[ignore]` test prints it).

## Notes

- The handle's `Arc<dyn SessionState>` is what M03-02b holds per session;
  it must survive eviction (the handle is a name, the sequence is a
  cache). Do not tie the handle's lifetime to a sequence.
- `generate(commit=false)` on the KV backend is cheap; on a recurrent
  model it costs a state save/load per query — the cost table makes that
  visible before M04-09 compares the two families.
