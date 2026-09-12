# M2 — Local inference service and model manager

Outline only. Full task files are written when the milestone starts (after
M01's two-week trial). Goal and exit criterion: `../../ROADMAP.md`.
Spec: `SPEC.md` §7 (inference service), §8 (base model selection).

## Planned tasks

| Id | Title | One-line goal |
|---|---|---|
| M02-01 | llama.cpp binding crate | `crates/llama/` (`apprentice-llama`): the only crate allowed `unsafe`; wraps llama.cpp via `llama-cpp-2` (or a vendored `llama-cpp-sys` build) with CUDA/Metal/Vulkan/CPU features; loads GGUF, tokenises, runs batched decode; builds on all three OSes. |
| M02-02 | Hardware detection | Detect GPUs (VRAM, driver, backend availability), CPU cores/RAM; produce a `HardwareProfile` used to choose quantisation, GPU layers, context size and slot count; `harness doctor` shows it. |
| M02-03 | Model manager and manifest | `<data_dir>/models/` with a `manifest.toml` per model/adapter: source URL, sha256, size, licence, quant, context, rated roles + eval scores; `harness models list\|pull\|remove\|assign <role> <model>`; resumable downloads from Hugging Face with verification. |
| M02-04 | Inference service | Single in-daemon service: request queue with priorities and per-role latency budgets, N slots with continuous batching, prefix (KV) cache for role prompts, per-agent sequence reuse, cancellation, graceful model swap; `InferenceRequest {role, prompt, params, budget, priority}` → stream of tokens. |
| M02-04b | Session state handle | `SessionState { append, generate, snapshot, restore, fork, drop }` (SPEC §7) over the service: one resident state per session for the observer (SPEC §5.0); KV-cache backend first (llama.cpp sequence copy / `llama_state_seq_*`), recurrent-state backend behind the same trait when a recurrent/hybrid model is loaded; snapshot bytes stored as trace blobs (kind `apprentice.state`); memory accounting per resident state and an eviction policy that demotes a session to the stateless fallback. |
| M02-05 | Bypass and back-pressure | Deadline scheduling: a request that cannot start within its budget is rejected immediately (`Bypassed{reason}`) rather than queued; metrics for queue depth, bypass rate, p50/p95 per role; exposed via `stats.apprentice` (fills the shape reserved in M00-07). |
| M02-06 | Small-model runners | ONNX Runtime (or `candle`) path for encoder/classifier models (rankers, gates) with millisecond latency; same `InferenceService` façade; model manifest supports `kind = "encoder"`. |
| M02-07 | Load benchmark | `harness models bench --agents N --roles ...`: simulated multi-agent load replaying real prompt sizes from traces; reports tokens/s, slot utilisation, latencies, bypass rate; JSON output stored under `<data_dir>/bench/`. |
| M02-08 | GUI model manager | Download/assign/remove models, hardware panel, live service metrics (queue, slots, bypass rate), warnings when VRAM is insufficient. |
| M02-09 | Remote inference endpoint (optional) | The same service exposed over the RPC transport on a second machine (`inference.remote = "host:port"`), token-authenticated; defer if time is short (also listed under M10). |

## Interfaces this milestone relies on (must exist from M00/M01)

- `AppState` in the daemon can host a long-lived service with its own
  thread pool (M00-08); config layer for `[inference]` (M00-03).
- `stats.tokens` result has the `apprentice` sub-object and
  `mentor_calls.apprentice_applied` column (M00-06/07).
- `harness doctor` hardware section is a stub to be replaced (M00-04).
- Trace event kind `apprentice.invocation` reserved (M00-06) — M02 records
  service-level invocations there even before roles exist (M03), e.g. for
  bench runs.

## Decisions to make at start

- Binding crate choice after a build spike on Windows (CUDA), macOS (Metal)
  and Linux (Vulkan/CPU).
- Candidate base models to pull for M03/M04: Qwen2.5-Coder 3B/7B, Qwen3
  4B/8B text-only, at Q4_K_M and Q5_K_M.
- Default slot count and context per slot for the 12 GB reference GPU:
  budget = weights + Σ slots × context × per-token KV bytes (depends on the
  model's layer count, KV heads, head dim and KV quantisation). The bench
  (M02-07) must report this table, not just tokens/s.
- KV-cache lifecycle: prefix sharing across slots for role prompts, slot
  save/restore for parked sessions, eviction policy when agents exceed slots.
  Whether the chosen binding exposes these (llama.cpp `llama_state_*` /
  server `/slots` save-restore) is part of the build spike.
- Whether to include a recurrent/hybrid candidate (RWKV-7, Falcon-H1,
  Nemotron-H, Qwen3-Next-class) in the pulled model set, depending on
  llama.cpp support at the time — and whether its state save/restore/copy is
  exposed by the binding (needed for M02-04b's recurrent backend).
- Snapshot cost table per candidate: bytes per session state at the chosen
  context (transformer: KV size × tokens; recurrent: fixed), snapshot/restore
  latency. This decides how often the observer snapshots (every step vs
  sampled).
