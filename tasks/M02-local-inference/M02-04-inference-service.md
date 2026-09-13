# M02-04 — Inference service

Status: todo
Depends on: M02-01, M02-02, M02-03
Size: L

## Goal

`apprentice_core::inference::InferenceService`: one shared, in-daemon
service that loads the assigned model according to the planner, serves
many concurrent requests through N slots with continuous batching, reuses
cached prefixes (per slot and across slots for shared role prompts),
streams tokens, supports cancellation and graceful model swap, and
records every invocation in the trace. Request shape:
`InferenceRequest {role, prompt, params, budget, priority}` → a stream of
tokens and a final usage record. Priorities and deadlines exist in the
types here; their enforcement is M02-05.

## Context

SPEC §7 (single shared service; continuous batching with slots;
per-agent KV reuse; prefix caching for role prompts; append-only prompts
so the cache survives across steps; priority queue), §3 ("Core runs as a
daemon … one local inference service"). M03's orchestrator calls this
from the `StepHooks`; M02-07's bench drives it under load; M02-04b builds
resident session states on it. M00-06 reserved the `apprentice.invocation`
event kind for what the service records.

## Scope

In: service lifecycle (lazy load, unload, swap), the scheduler thread and
batching loop, slots and prefix reuse, request/response types, streaming,
cancellation, trace recording, `inference.status`, config, the
`llama` cargo feature in core.
Out: deadline rejection and priority preemption (M02-05, but the queue is
shaped for them here), resident states (M02-04b), encoders (M02-06),
remote endpoint (M02-09).

## Design

### Types (`inference/types.rs`, wire parts mirrored in `apprentice-api`)

```rust
pub struct InferenceRequest {
    pub role: String,                              // "compressor", "bench", … (free-form; stats group by it)
    pub prompt: Prompt,                            // Prompt::Text(String) | Prompt::Chat(Vec<ChatMessage>) | Prompt::Tokens(Vec<Token>)
    pub prefix: Option<PrefixKey>,                 // names a shared prefix (role prompt) the service may keep resident and seq_cp from
    pub params: SampleParams,                      // max_tokens, temperature, top_p, top_k, min_p, repeat_penalty, stop: Vec<String>, seed, grammar: Option<String>
    pub budget: Budget,                            // start_by: Option<Duration>, finish_by: Option<Duration>  (M02-05 enforces)
    pub priority: Priority,                        // Interactive | Background
    pub session: Option<SessionId>, pub agent: Option<AgentId>, pub step: Option<StepId>,   // for the trace record
    pub model: Option<ModelRef>,                   // None = the role's assignment (M02-03)
}
pub struct Response { pub tokens: mpsc::Receiver<Piece>, pub done: oneshot::Receiver<Result<Usage, InferenceError>> }
pub enum Piece { Text(String), Token(Token) }
pub struct Usage { pub prompt_tokens: u32, pub cached_tokens: u32, pub generated: u32, pub queue_ms: u32, pub prefill_ms: u32, pub decode_ms: u32, pub slot: u32, pub model: String, pub adapter: Option<String>, pub stop: StopReason /* Eos | MaxTokens | StopString | Cancelled | Budget */ }
pub enum InferenceError { Disabled, NoModel{role}, Loading, Bypassed{reason: BypassReason} /* M02-05 */, PromptTooLong{tokens, max}, Backend(String), Cancelled }
```

### Service

```rust
pub struct InferenceService { .. }                // in AppState: state.inference() -> &InferenceService
impl InferenceService {
    pub async fn generate(&self, req: InferenceRequest, cancel: CancellationToken) -> Result<Response, InferenceError>;
    pub async fn tokenize(&self, text: &str) -> Result<Vec<Token>>;           // M03-09 needs counts
    pub async fn ensure_loaded(&self, model: &ModelRef) -> Result<LoadedInfo>;
    pub async fn reload(&self, model: &ModelRef, plan: Option<Plan>) -> Result<()>;   // drain → unload → load
    pub async fn unload(&self);
    pub fn status(&self) -> ServiceStatus;                                     // loaded model, plan, slots busy/total, queue depth, cells used, uptime, last error
}
```

Lifecycle: nothing is loaded at daemon start. The first `generate` (or
`ensure_loaded`) resolves the role's assignment (M02-03), runs the planner
(M02-02) with the config's wanted slots/ctx, loads the model on a
dedicated **scheduler thread** (`std::thread`, named `inference`), and
answers `Loading` to callers that arrive meanwhile unless their budget
allows waiting (M02-05 decides; in M02-04 they wait). One model loaded at
a time in M02; a request naming another model triggers a swap only if
`inference.auto_swap = true`, else `NoModel`. `inference.idle_unload_min`
(default 0 = never) unloads after idleness; `daemon.shutdown` drains
running generations for up to 3 s then drops the context.

### Scheduler thread and the batching loop

The thread owns the `apprentice_llama::Context` (it is `!Sync`) and a
`Vec<Slot>`; it talks to the async side through a bounded `crossbeam`/
`std::sync::mpsc` command channel (`Submit(req, tx)`, `Cancel(id)`,
`Reload(..)`, `Snapshot/Restore/Fork` for M02-04b, `Status`) and per-
request `tokio::sync::mpsc` token channels.

```
loop:
  drain commands (non-blocking); assign waiting requests to free slots (prefix match first, see below)
  batch.clear()
  for slot in slots:
     Prefill(remaining) → push up to n_ubatch tokens of the remaining prompt (pos continues after the reused prefix); the last one wants logits when it ends the prompt
     Generating          → push the last sampled token at pos; wants logits
  if batch is empty: block on the command channel (with the idle timer)
  decode(batch)  — on NoKvSlot: halve the prefill chunk of the largest slot and retry; on error: fail the affected requests, log, continue
  for each slot that got logits: sample → detokenize incrementally (UTF-8 boundary buffer) → send Piece; check stop strings (on the decoded tail), max_tokens, cancel token, EOS → finish slot with Usage
```

Every generating slot advances one token per iteration and prefills
share the batch, which is llama-server's continuous batching. `n_batch`
bounds the batch, `n_ubatch` the prefill chunk; both from the plan.

### Slots and prefix reuse

`Slot { seq: SeqId, tokens: Vec<Token> /* what the KV holds */, state: Free | Prefill | Generating, last_used, request }`.

Assigning a request: tokenize the prompt (on the scheduler thread; cached
by prompt hash for repeated benches), then pick the free slot whose
`tokens` share the longest common prefix with the prompt; `seq_rm` the
divergent tail, set `pos` to the common length, prefill the rest.
Shared prefixes (`req.prefix = Some(key)`): the first request with a key
prefills it into a **reserved sequence** (`n_seq_max = slots + resident
+ reserved`, reserved = `inference.shared_prefixes`, default 4, LRU by
key); later requests `seq_cp` it into their slot (cost: a memory copy,
no compute) when the slot's own prefix is shorter. Cached tokens are
reported in `Usage.cached_tokens`, so M03 can show how much of a role
prompt was free.

Append-only rule (SPEC §6): a request whose prompt extends the slot's
previous prompt costs only the delta — the common case for a role that
appends per step. The scheduler logs, at `debug`, the reuse ratio per
request; the bench aggregates it.

### Trace record

Every finished or failed request writes `apprentice.invocation`
`{role, model, adapter, protocol_version: null, latency_ms, queue_ms,
prefill_ms, decode_ms, bypassed: false, reason?, tokens_in,
tokens_cached, tokens_out, slot, stop, request_hash}` through the
`TraceWriter`, with `session/agent/step` when the request carried them
(bench requests carry a synthetic session created by M02-07). The
prompt/output pair goes to a blob only when `trace.capture_apprentice_io
= true` (default on: it is training data; the bench turns it off).

### Config (`[inference]`, extending M02-02's keys)

`enabled = true`, `auto_swap = false`, `idle_unload_min = 0`,
`shared_prefixes = 4`, `n_batch = 512`, `n_ubatch = 256`,
`flash_attention = "auto"`, `capture_io = true`, `load_timeout_s = 120`.
`apprentice.enabled` (M00-03) stays the switch for the *roles* (M03); the
service itself is on when a model is assigned.

### Surfaces

- RPC: `inference.status` (the `ServiceStatus`), `inference.load {model?}`,
  `inference.unload`, `inference.generate {role, prompt, params?}` →
  `{subscription}` streaming `inference.token {text}` and
  `inference.done {usage}` — the CLI's `harness apprentice run "<prompt>"
  [--role R] [--model M]` for manual poking; the GUI uses it in M02-08.
- `daemon.status` gains `inference: {model, slots_busy, slots}`; the
  daemon's idle timer treats a loaded model with running requests as
  activity.

### Core feature flag

`apprentice-core` feature `llama` (on in `harnessd`'s default features)
gates `inference::backend::llama`; without it the service exists but
answers `InferenceError::Disabled("built without llama")`, so
`cargo test -p apprentice-core` in CI runs the service's queue/slot logic
against a **`MockBackend`** (`inference::backend::mock`: a `Backend`
trait with `tokenize`, `decode`, `sample`, `seq_*`, `state_*` over a
fake tokenizer and scripted outputs, with configurable per-token delay)
— the same trait the llama backend implements.

## Acceptance

- [ ] Against the mock backend: 8 concurrent requests on 4 slots
      complete with their scripted outputs in order; the trace has one
      `apprentice.invocation` per request with the right role, tokens and
      slot; cancellation mid-generation frees the slot within one
      iteration and records `stop: cancelled`.
- [ ] Prefix reuse: a second request extending the first's prompt reports
      `cached_tokens == len(first prompt)`; two requests sharing a
      `prefix` key show the second cached from the reserved sequence and
      the mock records one prefill of the shared part.
- [ ] Stop strings and `max_tokens` end generation exactly (the stop
      string is not emitted); UTF-8 multi-byte tokens are emitted whole.
- [ ] Reload while requests run: running ones finish (or are cancelled
      after the drain timeout), queued ones run on the new model; status
      reports the swap.
- [ ] With the real backend and the 3B candidate on the reference
      machine (`#[ignore]`, `HARNESS_GPU=1`): 4 simultaneous 512-token
      prompts + 128-token generations complete with aggregate decode
      ≥ 3× a single stream's tokens/s; `memory_used()` after load is
      within the planner's table (M02-02 acceptance).
- [ ] CPU-only (`HARNESS_CPU=1`, the CI runners): the same test passes
      with relaxed timing; the daemon stays responsive to RPC
      (`daemon.status` < 50 ms) during generation.
- [ ] `harness apprentice run "hello" --role bench` streams tokens from
      the daemon and `harness trace list --kind apprentice.invocation`
      shows the record.

## Verification

`cargo test -p apprentice-core inference::` (mock backend, always);
`cargo test -p apprentice-core --features llama --test inference_gpu --
--ignored` on the reference machine and the CPU variant on the CI
runners (a nightly job, not per PR — the model download and the timings
do not belong in the PR gate).

## Notes

- The scheduler thread is the only place that touches llama.cpp; the
  async side only moves messages. Keep it that way — it is also what
  makes M02-09's remote backend a drop-in (the same command channel over
  a socket).
- Sampling on the scheduler thread is fine at 3–8B; if it ever shows up
  in the profile, move logits out and sample on the async side.
- Do not add role logic here (prompt templates, what to compress). The
  service knows tokens and slots; M03 knows roles.
