# M2 — Local inference service and model manager

Goal and exit criterion: `../../ROADMAP.md`. Spec: `SPEC.md` §7
(inference service), §8 (base model selection). Task files written
2026-09-13 (format: `../README.md`).

## Tasks

| Id | File | Size | Depends on |
|---|---|---|---|
| M02-01 | [llama.cpp binding crate](M02-01-llama-binding-crate.md) | L | M00-01, M00-12 |
| M02-02 | [Hardware detection and the memory planner](M02-02-hardware-detection-and-planner.md) | S | M00-04, M02-01 |
| M02-03 | [Model manager and manifest](M02-03-model-manager-and-manifest.md) | M | M00-03, M00-06, M02-02 |
| M02-04 | [Inference service](M02-04-inference-service.md) | L | M02-01, M02-02, M02-03 |
| M02-04b | [Session state handle](M02-04b-session-state-handle.md) | M | M02-04 |
| M02-05 | [Bypass and back-pressure](M02-05-bypass-and-back-pressure.md) | S | M02-04 |
| M02-06 | [Small-model runners (encoders and classifiers)](M02-06-small-model-runners.md) | M | M02-03, M02-04 |
| M02-07 | [Load benchmark](M02-07-load-benchmark.md) | M | M02-04, M02-04b, M02-05 |
| M02-08 | [GUI model manager](M02-08-gui-model-manager.md) | M | M01-12, M02-02, M02-03, M02-05, M02-07 |
| M02-09 | [Remote inference endpoint (optional)](M02-09-remote-inference-endpoint.md) | M | M02-04, M02-05, M02-06 |

Suggested order: 01 → 02 → 03 → 04 → 05 → 04b → 07 (first bench, exit
criterion) → 06 → 08 → 09 (if time allows). The id `M02-04b` is kept
because the M03/M04 outlines already reference it.

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
