# apprentice-harness — Specification

Project name: **apprentice-harness** (CLI binary `harness`).

## 1. Vision

A system-agnostic desktop application (GUI + CLI) that runs agentic coding work
with a remote foundation model, where a **small local model** does as much of
the work as possible so that the dialog with the remote model is as short and
as token-cheap as possible — without lowering task success.

Terminology used throughout:

| Term | Meaning |
|---|---|
| **Mentor** | The remote foundation model (initially Claude Opus 5 via the Anthropic API). |
| **Apprentice** | The local model(s) running on the user's machine (GPU or CPU, a few GB). |
| **Role** | A narrow, measurable job the apprentice performs (see §5). |
| **Agent** | One running task loop (mentor + apprentice + tools) on a workspace. |
| **Session** | A user-visible conversation containing one or more agents. |
| **Trace** | The complete, replayable record of everything an agent did. |
| **Trajectory** | One agent's trace from task start to end. |

The mentor and apprentice are both told about the relationship: the mentor
instructs and evaluates; the apprentice prepares, filters, compresses and
executes. The channel between them is not shown to the user by default, so it
is allowed to be terse, structured and technical.

## 2. Goals and non-goals

### Goals

1. A daily-usable coding assistant comparable in feel to the Claude desktop
   app: chat, workspaces, tool use (files, shell, search), streaming, permission
   prompts, session history.
2. **Complete trace capture from day one**, so a baseline exists before any
   local model is involved.
3. A local inference service that runs a coding-capable model of a few GB,
   serves multiple agents concurrently, and **never stalls** the coding loop
   (bypass to remote on overload).
4. A set of apprentice roles that measurably reduce remote tokens per
   successfully completed task.
5. An evaluation engine that produces a trustworthy token-savings number
   (net, including retries caused by the apprentice) and gates every change to
   prompts, models and adapters.
6. A training pipeline (Python) that turns traces and feedback into better
   apprentice models, runnable on the local GPU or rented compute, scheduled
   into idle periods.
7. CLI reaching feature parity with the GUI, so everything is scriptable.

### Non-goals (for now)

- Replacing the mentor. The apprentice reduces tokens; it does not aim to
  solve hard reasoning steps.
- Training a base model from scratch (see §8 for rationale).
- Non-Anthropic providers (planned later via an OpenAI-compatible adapter).
- Productivity/non-coding tools (planned later; the tool system must not
  preclude them).
- Cloud-hosted or multi-user deployment. This is a desktop, single-user app.

## 3. Architecture

```
┌──────────────────────────────┐   ┌──────────────────────────────┐
│  GUI (Tauri, web frontend)   │   │  CLI (`harness ...`)         │
└──────────────┬───────────────┘   └──────────────┬───────────────┘
               │  JSON-RPC over local IPC / stdio (same API)      │
┌──────────────┴──────────────────────────────────┴───────────────┐
│  Core (Rust library crate + daemon)                              │
│                                                                  │
│  Session/Agent runtime  ─  Tool system  ─  Permission engine     │
│  Mentor adapter (Anthropic)  ─  Apprentice orchestrator (roles)  │
│  Local inference service (llama.cpp)  ─  Model manager           │
│  Trace store  ─  Feedback store  ─  Token accounting             │
│  Eval engine  ─  Idle scheduler  ─  Config                       │
└──────────────────────────────────────────────────────────────────┘
┌──────────────────────────────────────────────────────────────────┐
│  ML workspace (Python): dataset builders, trainers, evaluators,  │
│  compute backends (local GPU / SSH / SkyPilot), adapter registry  │
└──────────────────────────────────────────────────────────────────┘
```

Principles:

- **One core, thin clients.** GUI and CLI are both clients of the same core
  API. Feature parity is a checklist over that API, not a rewrite.
- **Core runs as a daemon.** Multiple sessions and agents share one process,
  one local inference service, one trace store. GUI and CLI attach to it.
- **Everything is a trace.** No code path calls the mentor or apprentice
  without writing to the trace store.
- **Roles are pluggable.** Each apprentice role has a defined input/output
  contract and can be implemented by a prompt over the base model, a fine-tuned
  adapter, a small dedicated model, or plain code — and swapped without
  touching the agent loop.
- **Fail open to the mentor.** Any apprentice failure (timeout, error, low
  confidence) degrades to the remote-only behaviour for that step.

### 3.1 Core components

**Session / agent runtime.** Owns the agent loop: build context → call mentor →
execute tool calls → repeat. Supports multiple concurrent agents (parallel
sessions, and sub-agents spawned by a parent agent). Persists sessions for
resume.

**Tool system.** Coding tools first: read/write/edit file, list/glob, grep,
shell command, git basics, run tests. Tools are declared with JSON schemas and
a *risk class* (read-only / write / execute / network). A tool can also be
executed by the apprentice on behalf of the mentor (see role *Executor*).
Designed so productivity tools (files outside repos, documents, browser) can be
added later without changing the runtime.

**Permission engine.** Per-tool, per-risk-class rules with modes (ask / allow
in workspace / allow always / deny), prompt-in-GUI and prompt-in-CLI.

**Mentor adapter.** Anthropic Messages API client: streaming, tool use, adaptive
thinking, effort level, prompt caching with stable-prefix layout, and full
`usage` capture (input, output, cache-read, cache-creation tokens) on every
response. The remote model id is configuration, never a constant.

**Apprentice orchestrator.** Decides, per step of the agent loop, which roles
run, in what order, with what latency budget, and merges their output into
the mentor request. Records for every role invocation: input, output, latency,
whether it was bypassed, and the token delta it produced.

**Local inference service.** See §7.

**Model manager.** Downloads/validates GGUF models and adapters, detects
hardware (VRAM, RAM, CPU), picks quantization/offload settings, maintains a
model manifest (which roles a model/adapter is rated for and its eval scores).

**Trace store.** Append-only, local (SQLite + blob files). See §9.

**Feedback store.** User ratings and mentor verdicts linked to trace events.
See §10.

**Token accounting.** Per call, per step, per agent, per session, per day:
remote input/output/cached tokens, estimated cost, apprentice tokens, bypass
count. Exposed to GUI and CLI.

**Eval engine.** Runs task corpora in A/B configurations, replays traces, and
produces reports. See §12.

**Idle scheduler.** Detects low/no load (no active agents, GPU idle, user idle
optional) and runs queued jobs: dataset builds, training runs, eval runs,
model downloads. Preempts when the user starts working.

**Config.** Layered (defaults → user → workspace), file-based, editable from
GUI/CLI, with secrets kept in the OS keychain.

## 4. The agent loop with an apprentice

Per step, in the *remote-only* baseline:

1. Assemble context (system prompt, history, tool results).
2. Call mentor. Receive text and/or tool calls.
3. Execute tool calls. Append results.
4. Repeat until the mentor ends the turn.

With the apprentice, each stage gets a hook:

| Stage | Apprentice roles that may run | Effect on remote tokens |
|---|---|---|
| Before call | History compactor, Context selector, Gate | Fewer input tokens; sometimes no call at all |
| Tool result | Output compressor | Fewer input tokens on the next call |
| Mentor tool request | Executor (mechanical steps done locally) | Fewer round trips |
| After call | Mentor verdict collection (feedback) | None (training signal) |

Every hook is optional, has a latency budget, and falls through cleanly.

## 5. Apprentice roles

Each role is specified by: input, output, success criterion, latency budget,
fallback, and how it is evaluated. Roles start as prompts and become trained
artifacts once data exists.

### 5.1 Output compressor
- **Input:** a tool result (file content, grep output, test log, diff, shell
  output) plus the current task summary.
- **Output:** a shorter representation that preserves what the mentor needs
  (errors with locations, signatures, changed regions, exact line numbers,
  exact strings that may be needed for edits).
- **Success:** the mentor's next action is unchanged compared with the full
  result (replay-equivalence, §12.3).
- **Fallback:** send the raw result (optionally with a mechanical truncation).
- **Note:** must be *lossless on demand* — the raw result stays available and
  the mentor can request it in full by reference (`expand <result-id>`).

### 5.2 Context selector
- **Input:** task description, current state summary, repository index
  (tree-sitter symbols, file list, recent edits), candidate chunks.
- **Output:** ranked list of chunks with a token budget applied.
- **Success:** the mentor succeeds at the step with the selected set; ablation
  shows selected chunks are used and omitted chunks are not needed.
- **Implementation candidates:** a small encoder/cross-encoder ranker (fast,
  cheap to train) with the generative model only for query formulation.

### 5.3 History compactor
- **Input:** the conversation so far.
- **Output:** a compact state document (goal, decisions, files touched, open
  problems, what was tried) used in place of old turns.
- **Success:** mentor behaviour equivalence on replay; no re-asking for facts
  already established.
- **Note:** complements, and can be compared against, the Anthropic
  server-side compaction feature.

### 5.4 Executor
- **Input:** a mechanical instruction from the mentor expressed in the protocol
  (e.g. "apply this edit", "find callers of X", "run tests, report failures").
- **Output:** a compact report; multi-step mechanical work done locally without
  a remote round trip per step.
- **Success:** correct execution and a report the mentor accepts; fewer round
  trips per task.
- **Safety:** obeys the same permission engine as mentor-issued tool calls.

### 5.5 Gate
- **Input:** the pending step.
- **Output:** decision: `remote` / `local` (executor can finish it) / `ask user`.
- **Success:** local decisions do not lower task success; remote tokens fall.
- **Note:** last role to enable; requires enough data to be trusted.
  Conservative by default.

### 5.6 Optional later roles
- **Vision checker** (separate small VLM) for screenshot-based test/UI checks.
- **Draft author** — local first draft of simple edits for mentor review.

## 6. Mentor–apprentice protocol (prompt engineering)

The protocol is a first-class, versioned artifact (`protocol/vN/`), consisting
of the mentor system prompt sections, the apprentice role prompts, and the
wire formats. Both parties are explicitly told:

- the mentor is the senior engineer and decision maker; the apprentice is a
  local assistant with limited capability and full access to the workspace;
- the channel is machine-to-machine: no pleasantries, no restating, structured
  blocks, references instead of repetition;
- the apprentice may be wrong; the mentor can always ask for the raw data.

Wire format principles:

- Compact tagged blocks rather than prose (e.g. `#ctx`, `#result`, `#state`,
  `#need`). Exact strings and line numbers are never paraphrased.
- Stable prefix layout so prompt caching is effective: tools, then frozen
  system text, then compacted state, then the volatile tail.
- The mentor can issue apprentice-directed requests (`#ask-apprentice`) that
  are handled locally and never appear as separate remote calls.
- At the end of a step, the mentor may be asked (cheaply, in the same call
  via a structured output) for a **verdict** on the apprentice's contribution:
  sufficient / missing X / over-compressed Y. This is training signal (§10).

Prompt changes are evaluated exactly like model changes (§12); no prompt is
promoted to default without a passing eval report.

## 7. Local inference service

Requirements driven by "multiple agents at the same time, must not slow
coding":

- Single shared service inside the core daemon, built on llama.cpp (GGUF),
  with CUDA/Metal/Vulkan/CPU backends selected by the model manager.
- Continuous batching with multiple sequence slots; per-agent KV-cache reuse
  where possible; prompt-prefix caching for role prompts.
- Priority queue: interactive roles (compressor on the critical path) before
  background roles (feedback labelling, idle-time work).
- **Latency budgets per role** and a **bypass rule**: if a request cannot be
  served within budget, the orchestrator proceeds without the apprentice for
  that step and records a bypass. Bypass rate is a tracked metric.
- Small dedicated models (rankers, gates) run in the same service or in-process
  (ONNX/candle) and are expected to cost milliseconds.
- Throughput benchmarking command reporting tokens/s, slot utilisation and
  p50/p95 latency per role under simulated N-agent load.
- Optional: use a second machine's service over the network (same API), for
  users with a separate GPU box.

## 8. Base model selection

Criteria:

1. **Text-only, coding-focused.** No audio/vision towers; weights spent on code
   and instruction following. Vision, if ever needed, is a separate model.
2. **1.5B–8B parameters**, Q4–Q5 GGUF, so that the 3–4B tier fits in ~3 GB
   (any modern GPU, or CPU) and the 7–8B tier fits in ~5 GB (8 GB+ GPUs).
3. **Permissive licence** allowing fine-tuning and redistribution of adapters
   (Apache-2.0 preferred).
4. **Good instruction following and long context** (≥32k) for compaction
   and selection roles.
5. **Trainable on 12 GB VRAM** with LoRA (3–4B) or QLoRA (7–8B).

Initial candidates: Qwen2.5-Coder (1.5B/3B/7B) and Qwen3 text-only (4B/8B).
The choice is made empirically in the eval engine; the model manifest supports
several models side by side and per-role assignment.

Why fine-tune rather than train from scratch: the apprentice's value comes from
understanding code, which pretraining provides at a cost of ~10⁵–10⁶ GPU-hours
for a 3B model. Harness traces will number thousands to hundreds of thousands
of examples — enough to steer a model, not to create one. Purpose-built
components that *are* trained from scratch on top of pretrained encoders
(rankers, gates) are part of the design where classification beats generation.

## 9. Trace capture

Captured from the first usable version, before any apprentice exists.

Per event (append-only, timestamped, linked to session/agent/step):

- Mentor request as sent (full messages, tools, system, model, thinking/effort
  settings) and response (content blocks, stop reason, `usage`).
- Tool calls and full raw results (with hashes; large blobs stored once).
- Apprentice invocations: role, input, output, latency, bypassed?, model/adapter
  version, protocol version.
- Workspace snapshot references (git commit / dirty diff) at task start and end.
- Task outcome signals: tests run and results, user accepted/rejected, user
  ended the task, errors.
- User feedback events (§10).

Storage: SQLite metadata + content-addressed blob directory, per user, local.
Export/import to a portable bundle for training and for sharing anonymised
corpora. Redaction hooks for secrets before export.

Replayability: any step can be reconstructed exactly (same prefix), which is
what the replay evaluator needs (§12.3).

## 10. Feedback and continuous improvement loop

Signals collected:

1. **User feedback** — in GUI and CLI: rate a turn or a task (good / bad /
   "the model lacked context" / "too slow"), free-text notes; implicit
   signals: accepted edits, reverted edits, repeated requests.
2. **Mentor verdicts** — structured judgement on each apprentice contribution
   requested inside the normal call (cheap; cached prefix) or in batch later:
   was the context sufficient, what was missing, what was unnecessary.
3. **Outcome signals** — tests passed, task completed, number of retries,
   tokens spent after an apprentice decision (regret).

Loop:

```
traces + feedback  →  dataset builder  →  training job (idle time)
      ↑                                          ↓
  production   ←  promotion gate (eval report)  ←  candidate adapter/prompt
```

- The **idle scheduler** runs dataset builds and training jobs when no agents
  are active and the GPU is free; jobs are checkpointed and preemptible.
- Candidates (adapters, ranker weights, prompt versions) are promoted only if
  the eval engine shows no loss in task success and a gain in net tokens.
- Everything promoted is versioned; roll back is one command.

## 11. Training pipeline (Python workspace)

- **Dataset builders** turn traces into per-role datasets:
  - compressor: (raw result, task state) → teacher-written compression, with
    replay-verified sufficiency labels;
  - selector: (task, chunks) → relevance labels from ablation replay and mentor
    citations;
  - compactor: (history) → teacher-written state document, replay-verified;
  - executor: (instruction, workspace) → action sequence + report from traces;
  - gate: (step) → remote-needed label from outcomes.
- **Trainers:** SFT with LoRA/QLoRA (PEFT/Unsloth) for generative roles;
  small encoder fine-tuning for rankers; later DPO/GRPO with reward =
  task success − λ·remote tokens, using paired trajectories.
- **Teacher generation** uses the mentor via the Batch API (50% cost) and
  prompt caching; the LLM-judge for equivalence scoring can be a cheaper model.
- **Compute backends:** one job spec runs on the local GPU, an SSH host, or
  cloud via SkyPilot. Artifacts (adapters, metrics, dataset hashes) land in one
  local registry; the model manager consumes them.
- **Reproducibility:** every job records dataset hash, base model, hyper-
  parameters, protocol version and the resulting eval report.

## 12. Evaluation and token-savings measurement

### 12.1 Task corpus
- Public: SWE-bench-Lite / Verified subsets, Aider polyglot, small synthetic
  repos with tests.
- Private: tasks recorded from the user's own sessions (replayable, with
  outcome signals), which reflect real usage.

### 12.2 Primary metrics
- **Remote tokens per successfully completed task** (input, output, cached
  reported separately; cost derived from pricing config).
- **Task success rate** (tests pass / judge / user acceptance) — must not fall
  beyond a configured threshold.
- **Round trips per task**, wall-clock per task, apprentice bypass rate.
- **Net savings** = baseline tokens − harness tokens, *including* retries and
  expansions caused by the apprentice (regret is charged).

### 12.3 Replay evaluator (cheap, deterministic, CI-grade)
Take a logged step, substitute the apprentice's output (compressed result,
selected context, compacted history), re-ask the mentor for that single step,
compare with the original action: exact match for tool calls/edits, judge for
text. Reports sufficiency rate per role and token delta per role. Runs in
minutes, not hours, and gates every prompt/model change.

### 12.4 Full A/B runs
Same corpus, remote-only vs remote+apprentice (and between apprentice
versions), several seeds, reported as `harness bench` output with confidence
intervals.

### 12.5 Regression gate
A change to a role implementation, prompt, adapter or base model is promoted
only if: sufficiency rate ≥ threshold, success rate not worse than baseline
within tolerance, and net tokens improved.

## 13. GUI requirements (Tauri)

- Chat with streaming, markdown/code rendering, diffs, tool-call cards with
  expand-to-raw.
- Workspaces (folders/repos), session list, resume, search.
- Permission prompts; per-workspace rules editor.
- Multiple concurrent agents/sessions with status.
- Token/cost panel per session and per day; apprentice contribution view
  (what was compressed/selected, how many tokens it saved, bypasses).
- Feedback controls on turns and tasks.
- Model manager UI: download, hardware detection, per-role assignment,
  adapter versions, rollback.
- Eval and training dashboards (reports, job queue, idle-scheduler status).
- Settings, secrets in keychain.

## 14. CLI requirements

Everything the GUI can do, scriptable. Indicative command set:

```
harness run "<task>" [--workspace P] [--no-apprentice] [--json]
harness chat                       # interactive TUI-lite
harness sessions list|show|resume|export
harness tools list|allow|deny
harness models list|pull|assign <role> <model|adapter>|rollback
harness trace show <id>|export|redact
harness feedback rate <turn> good|bad [--note]
harness bench [--corpus X] [--config A --config B] [--replay]
harness dataset build <role>
harness train <role> [--backend local|ssh:host|sky:cfg]
harness eval replay <trace-range>
harness idle status|pause|resume
harness stats tokens [--since]
harness daemon start|stop|status
```

## 15. Configuration, storage, privacy

- Config in TOML, layered; secrets in the OS keychain.
- All data local by default. Nothing leaves the machine except mentor API
  calls and explicitly triggered exports/cloud training jobs.
- Redaction pass on export; workspace-level "never capture" patterns.

## 16. Tech stack

- **Core:** Rust (tokio, serde, rusqlite, tree-sitter, llama-cpp bindings,
  tauri).
- **GUI:** Tauri 2 with a TypeScript web frontend.
- **CLI:** Rust binary sharing the core crate; talks to the daemon.
- **ML:** Python (PyTorch, PEFT/Unsloth, datasets, SkyPilot; ONNX export for
  small models).
- **Platforms:** Windows, macOS, Linux; GPU via CUDA/Metal/Vulkan; CPU fallback.

## 17. Decisions deferred

- Licence.
- API budget for teacher-data generation (decided at Milestone 5/6).
- Exact base model and quantization (decided empirically in Milestone 4).
- Whether server-side compaction/context-editing from the API is used as a
  baseline or complement to the local compactor.
- Vision role.
