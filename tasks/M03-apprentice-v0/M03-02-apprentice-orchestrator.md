# M03-02 — Apprentice orchestrator

Status: todo
Depends on: M01-08, M02-04, M02-05, M03-01
Size: L

## Goal

The `StepHooks` implementation that runs the apprentice: at each hook
point it decides which roles run for this step, calls them through the
inference service with the role's budget and priority, merges their
outputs into the conversation (a `#result` block in place of a raw tool
result, a `#state`/`#ctx` block before the call), records one
`apprentice.invocation` per role invocation with token deltas, the
backend that served it and the bypass reason when there was one, and
**fails open** — any error, timeout or bypass leaves the step exactly as
the baseline would have run it. Two role backends behind one interface:
**observer** (M03-02b, primary) and **stateless** (this task; the
fallback and the A/B baseline); `--no-apprentice` and the config toggle
select `NoopHooks`.

## Context

SPEC §4 (the hook table; "Every hook is optional, has a latency budget,
and falls through cleanly"; the two shapes "implement the same hooks and
produce the same protocol blocks, so the orchestrator does not care
which is active"), §3 ("never stalls the coding loop"). M01-08's
`StepHooks { before_call, before_tool_exec, on_tool_result }` with
`CallContext { conversation, step }`, `ToolResultContext { results,
step }` and `NoopHooks`; the runtime calls them at every step (`agent.rs
run_loop`). M02-04's `InferenceService::generate(InferenceRequest)` with
`role`, `prefix`, `budget`, `priority`, `session/agent/step`; M02-05's
`Bypassed{reason}`; `RunOptions.apprentice: Option<bool>` and
`apprentice.enabled` already exist; `ApprenticeStats` is reserved in
`stats.tokens`.

## Scope

In: the orchestrator, the `RoleBackend` interface and the stateless
backend, per-step role planning, the hook contexts' extension, the
invocation record, `apprentice_applied`, the config, the run/CLI/GUI
toggles, `#ask-apprentice` handling, tests against the mock inference
backend with fake roles.
Out: the roles' prompts and logic (M03-03/06/07 plug in as `Role`
impls), the observer backend (M03-02b), the state injection mechanics
(M03-08 — the orchestrator calls an `Injector`), token estimation
(M03-09 — the orchestrator records the sizes it is given).

## Design

### Module (`runtime/apprentice/`)

```
apprentice/
  mod.rs          Orchestrator: StepHooks impl, RolePlan, config → which roles, fail-open wrapper
  backend.rs      RoleBackend trait; Stateless backend; BackendKind
  roles.rs        Role trait (the M03-03/06/07 implementations register here) + RoleName
  record.rs       InvocationRecord builder → apprentice.invocation
  ask.rs          #ask-apprentice extraction from mentor text and the reply
```

```rust
pub trait Role: Send + Sync {
    fn name(&self) -> RoleName;                      // Compressor | Compactor | Selector | Observer (ingest only) | later Gate/Executor
    fn hook(&self) -> Hook;                          // BeforeCall | OnToolResult
    fn wants(&self, ctx: &StepView) -> Option<Want>; // None = skip this step (e.g. compressor: result below min_bytes); Some(Want{priority, budget_override})
    async fn run(&self, ctx: &mut StepView<'_>, backend: &dyn RoleBackend, cancel: CancellationToken) -> Result<RoleOutcome, RoleError>;
}
pub struct RoleOutcome { pub blocks: Vec<wire::Block>, pub usage: Usage, pub tokens_before: u32, pub tokens_after: u32, pub verbatim_bytes: u32, pub notes: Value }

pub trait RoleBackend: Send + Sync {
    fn kind(&self) -> BackendKind;                   // Observer | Stateless
    async fn query(&self, role: RoleName, q: Query, budget: Budget, priority: Priority, cancel) -> Result<Answer, InferenceError>;
    async fn ingest(&self, ev: &SessionEvent) -> Result<(), InferenceError> { Ok(()) }   // observer only
}
```

`Query { prompt: Prompt /* the role prompt section + the assembled input, or for the observer just the query */, prefix: Option<PrefixKey> }` — the
stateless backend sends `prefix = Some(role prompt hash)` so the service
`seq_cp`s the role prompt (M02-04's shared prefixes) and only the input
is prefilled per call.

### Hook contexts (extending M01-08)

`CallContext` and `ToolResultContext` gain a `StepView`: `session_id`,
`agent_id`, `step_id`, the `Arc<AppState>` (trace store/writer, workspace
handle, inference service, config), the run's `RunOptions`, and for
`on_tool_result` the executed calls with their `blob_id`, `metadata` and
`output_bytes`. `Conversation` gains `insert_before_call(Message)`
(M03-08 decides its shape) and `replace_tail(from_row, Vec<Message>)`
(M03-07); both keep `session_messages` in sync through the runtime.

### Planning a step

```
before_call:   if step == 1 or state.changed → Compactor? (M03-07's trigger: context_tokens ≥ trigger) → Selector? (M03-06's trigger) → Injector writes the blocks
on_tool_result: for each Executed with a blob and output_bytes ≥ compressor.min_bytes → Compressor (parallel across results, one request each, Interactive)
```

Roles run in the order above; a role that bypasses does not stop the
others. Every invocation is wrapped: `tokio::time::timeout(finish_by +
grace)`, `catch_unwind` is not used (no panics escape the service), any
`Err` → the baseline path for that role + a record with `bypassed: true,
reason` (`Deadline`, `ModelLoading`, `Unavailable`, `RoleError{kind}`,
`InvalidOutput` when the parser rejects the block, `RefsInvalid` when
M03-03's validation drops it). Priority: compressor and selector
`Interactive`; compactor `Background` when triggered early (below
`compactor.urgent_tokens`), else `Interactive`; observer ingest
`Background`.

`#ask-apprentice` lines in the mentor's text blocks are extracted
(`ask.rs`), removed from what the user sees (`assistant.message` keeps
the original), answered by the backend (`Query::Ask`) in the next
tool-result turn as a `#result` block tagged `ask`, and recorded as an
invocation with `role: "ask"`. Without tool calls in that turn, the
answer goes into the synthetic user turn that continues the run.

### Backend selection

```toml
[apprentice]
enabled = false
backend = "auto"            # auto (observer when M02-04b has a resident state, else stateless) | observer | stateless
roles = ["compressor"]      # day-one default; compactor, selector added per the README's order
ask = true                  # honour #ask-apprentice
[apprentice.compressor]  enabled = true   min_bytes = 2048   …(M03-03)
[apprentice.compactor]   enabled = true   …(M03-07)
[apprentice.selector]    enabled = false  …(M03-06)
```

`RunOptions.apprentice: Some(false)` → `NoopHooks` for that run (the
session's `protocol_version` stays as set at the first run; M03-01's
prefix-changed event covers a toggle between runs). `backend = "auto"`
asks `StateManager::get(session)`; demotion (M03-02b) flips the session
to stateless until the next successful `open`. The active backend is in
every record.

### Record (`apprentice.invocation`, superset of M02-04's)

```json
{ "role": "compressor", "hook": "on_tool_result", "backend": "stateless", "protocol_version": "v1",
  "model": "…", "adapter": null, "bypassed": false, "reason": null,
  "latency_ms": 312, "queue_ms": 4, "prefill_ms": 190, "decode_ms": 118,
  "tokens_in": 1840, "tokens_cached": 620, "tokens_out": 96,
  "tokens_before": 2210, "tokens_after": 140, "verbatim_bytes": 410, "estimated_saved": null,   ← M03-09 fills estimated_saved
  "input_ref": "blob:…", "output_ref": "blob:…",           ← under trace.capture_apprentice_io
  "target": {"tool_call_id": "…", "blob_id": "…"} | {"rows": [a, b]} | {"chunks": n} }
```

Written through the `TraceWriter` before the step's next mentor request
is recorded (so a replay (M04) sees the invocation, then the request that
used it); `mentor_calls.apprentice_applied = 1` when at least one role
changed the request that step; `agent.apprentice` event (M03-10) mirrors
the record to clients.

### Surfaces

- CLI `harness run --no-apprentice` / `--apprentice` (sets
  `RunOptions.apprentice`), `harness apprentice status` (M02-05) gains
  `roles: [..], backend`.
- GUI: the composer's run options (M03-10 adds the visible toggle; this
  task exposes it through `agent.run`).
- `harness trace show --apprentice` prints the invocations of a session
  per step.

## Acceptance

- [ ] With `apprentice.enabled = false` or `--no-apprentice`, the runtime
      uses `NoopHooks` and no `apprentice.invocation` is written; the
      request bytes of a step equal the baseline's (golden test).
- [ ] Mock inference backend + a fake compressor role that returns a
      fixed `#result` block: a step with two large tool results runs two
      invocations in parallel, both records are written before the next
      `mentor.request`, `apprentice_applied = 1`, the request contains
      the blocks in place of the raw text.
- [ ] Fail-open: the fake role returns `Err`, panics are not possible,
      the service answers `Bypassed{Deadline}`, the service is
      `Disabled`, and the parser rejects the output — in each case the
      step's request equals the baseline's and a record with the reason
      exists; the loop's step timing grows by no more than the budget.
- [ ] `#ask-apprentice what does foo() return?` in mentor text is
      removed from the transcript shown to the client, answered in the
      next user turn as a `#result` block, and recorded with `role:
      "ask"`.
- [ ] `backend = "auto"` picks the observer when `StateManager::get`
      returns a state and stateless otherwise; a demotion mid-session
      switches the records' `backend` field.
- [ ] Live on the reference machine with the day-one roles: a 10-step
      dogfood session shows compressor invocations on every large tool
      result, bypass rate < 5 %, and no step waited longer than the
      compressor's `start_by` on the apprentice (M02-05's metrics).

## Verification

`cargo test -p apprentice-core runtime::apprentice::` (mock backend and
fake roles; no llama, no network); the live dogfood session with its
`harness stats apprentice` output in the completion notes.

## Notes

- The orchestrator is the only place that knows the hook order and the
  fail-open policy; roles never touch the conversation directly — they
  return blocks, the orchestrator (with M03-08's injector) places them.
  That keeps the append-only rule (SPEC §6) enforceable in one spot.
- Do not make the observer a hard requirement anywhere in this task; the
  README's fallback plan ("if M02-04b is not ready the stateless backend
  ships first") must be a config change, not a code change.
