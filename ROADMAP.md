# apprentice-harness — Roadmap

Milestones only; detailed tasks are created when a milestone starts.
Each milestone has a goal and an exit criterion. Order reflects priority:
a usable, trace-capturing harness first; the apprentice is layered on top.

## M0 — Foundations
Goal: repository skeleton that all later work fits into.
- Rust workspace: `core` library, `daemon`, `cli`, `gui` (Tauri) crates;
  Python `ml/` workspace.
- Core API (JSON-RPC) surface defined; GUI and CLI both attach to the daemon.
- Layered config, keychain secrets, logging.
- Anthropic mentor adapter with streaming, tool use, thinking/effort, `usage`
  capture, cache-friendly prefix layout.
- Trace store schema (SPEC §9) and token accounting.

Exit: `harness daemon start` + `harness run "hello"` round-trips through the
mentor and the full exchange is in the trace store.

## M1 — Usable coding assistant (remote-only) with full capture
Goal: something you use every day instead of the Claude desktop app, so real
traces start accumulating.
- GUI: chat, streaming, markdown/diff/tool cards, workspaces, session
  persistence and resume, permission prompts.
- Coding tools: read/write/edit, glob, grep, shell, git basics, run tests.
- Permission engine with per-workspace rules.
- Every mentor call, tool call and outcome captured and replayable.
- Session/day token and cost panel.

Exit: used for real work for at least two weeks; trace export produces a
corpus; a replay of any recorded step reproduces the exact mentor request.

## M2 — Local inference service and model manager
Goal: the apprentice can run — robustly, alongside many agents.
- llama.cpp integration with CUDA/Metal/Vulkan/CPU backends.
- Model manager: GGUF download/verify, hardware detection, quantization and
  offload selection, model manifest with per-role assignment.
- Shared service with continuous batching, slots, priority queue, per-role
  latency budgets and bypass.
- `harness models bench` reporting tokens/s and p50/p95 latency under
  simulated N-agent load.

Exit: two candidate base models run concurrently for 4+ simulated agents
without exceeding role latency budgets on the reference machine (RTX 3080 Ti
12 GB) and on CPU-only.

## M3 — Apprentice v0: prompted roles and protocol v1
Goal: first token savings from prompt engineering alone, no training.
- Mentor–apprentice protocol v1 (system prompt sections, tagged wire format,
  `expand` by reference, `#ask-apprentice`).
- Roles implemented as prompts: output compressor, history compactor,
  context selector (with tree-sitter repo index).
- Orchestrator hooks in the agent loop; per-role trace records; A/B toggle
  (`--no-apprentice`) in GUI and CLI.
- Apprentice contribution view in the GUI (what was done, tokens saved,
  bypasses).

Exit: apprentice runs on every step by default with bypass rate < 5% and no
observed stalls; the GUI shows per-step token deltas.

## M4 — Evaluation engine
Goal: a trustworthy number for "does the apprentice lower tokens?".
- Task corpus: public subsets (SWE-bench-Lite/Verified, Aider polyglot) plus
  private replayable tasks from own traces.
- Replay evaluator (per-role sufficiency and token delta; minutes to run).
- Full A/B runner (`harness bench`) with multiple seeds and confidence
  intervals; net-savings accounting including regret.
- Regression gate definition and report format.
- Base model and quantization chosen empirically.

Exit: first published report: remote-only vs apprentice v0, with success rate,
net tokens per successful task and round trips; gate wired to protocol and
model changes.

## M5 — Feedback loop and idle scheduler
Goal: collect training signal continuously and be able to use idle time.
- User feedback controls (GUI/CLI) and implicit signals (accepted/reverted
  edits, retries).
- Mentor verdicts on apprentice contributions (structured output in-call or
  batch).
- Dataset builders per role (Python) reading the trace/feedback stores,
  replay-verified labels.
- Idle scheduler: load detection, job queue, preemption, GUI status.

Exit: datasets for compressor, compactor and selector are built automatically
from real usage; idle jobs run and yield to interactive work.

## M6 — Training pipeline v1: distillation
Goal: the first trained apprentice beats the prompted one on the gate.
- Compute backend abstraction: local GPU, SSH host, SkyPilot cloud; one job
  spec, one artifact registry.
- SFT/LoRA (and QLoRA for 7–8B) for compressor and compactor; encoder-based
  ranker for context selection.
- Adapter registry, promotion through the regression gate, one-command
  rollback in the model manager.
- API budget for teacher data decided here; Batch API and caching used for
  teacher generation.

Exit: at least one trained role promoted to default after passing the gate,
with the report showing improvement over the prompted role.

## M7 — Multi-agent orchestration and Executor role
Goal: fewer round trips through local execution and parallelism.
- Parallel sessions and sub-agents sharing the inference service.
- Executor role: mechanical multi-step instructions carried out locally under
  the permission engine, compact reports back.
- Scaling checks against M2 latency budgets under real multi-agent load.

Exit: round trips per task measurably reduced on the corpus with no drop in
success; bypass rate stays within budget with concurrent agents.

## M8 — CLI parity and headless mode
Goal: everything in the GUI is scriptable.
- Command set from SPEC §14 complete; JSON output for all commands; headless
  daemon operation for CI-like usage.
- Parity checklist generated from the core API and kept green.

Exit: parity checklist passes; a full bench + train + promote cycle runs from
the CLI alone.

## M9 — Preference optimisation and Gate role
Goal: optimise the real objective, not proxies.
- Paired trajectories from A/B runs; DPO/GRPO with reward =
  success − λ·remote tokens.
- Gate role (remote / local / ask) trained on outcomes; conservative rollout.
- Protocol v2 informed by accumulated mentor verdicts.

Exit: net tokens per successful task improved over M6 on the corpus with
success rate within tolerance; gate enabled by default.

## M10 — Extensions
Goal: broaden without disturbing the core.
- OpenAI-compatible provider adapter (second mentor; cross-mentor
  generalisation checks in the eval engine).
- Productivity tool set (files outside repos, documents, browser) as plugins.
- Optional vision role (separate small VLM) for screenshot-based checks.
- Remote inference service on a second machine.

Exit: decided per extension; each ships with its own eval evidence.
