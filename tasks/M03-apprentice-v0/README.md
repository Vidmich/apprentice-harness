# M3 — Apprentice v0: prompted roles and protocol v1

Goal and exit criterion: `../../ROADMAP.md`. Spec: `SPEC.md` §4 (hook
points), §5 (roles), §6 (mentor–apprentice protocol). Task files written
2026-09-13 (format: `../README.md`).

## Tasks

| Id | File | Size | Depends on |
|---|---|---|---|
| M03-01 | [Protocol v1 artifact](M03-01-protocol-v1-artifact.md) | M | M01-09, M00-06 |
| M03-02 | [Apprentice orchestrator](M03-02-apprentice-orchestrator.md) | L | M01-08, M02-04, M02-05, M03-01 |
| M03-02b | [Observer](M03-02b-observer.md) | M | M02-04b, M03-02 |
| M03-03 | [Role: output compressor](M03-03-role-output-compressor.md) | M | M03-02, M03-01, M01-01 |
| M03-04 | [`expand` by reference](M03-04-expand-by-reference.md) | S | M01-01, M03-01 |
| M03-05 | [Repository index](M03-05-repository-index.md) | M | M01-02 |
| M03-06 | [Role: context selector](M03-06-role-context-selector.md) | M | M03-02, M03-05, M03-08 |
| M03-07 | [Role: history compactor](M03-07-role-history-compactor.md) | M | M03-02, M03-08, M01-10 |
| M03-08 | [Mid-conversation state injection](M03-08-mid-conversation-state-injection.md) | S | M03-01, M01-08 |
| M03-09 | [Local token estimation](M03-09-local-token-estimation.md) | S | M03-02, M02-04, M01-13 |
| M03-10 | [GUI: apprentice contribution view](M03-10-gui-apprentice-contribution-view.md) | M | M01-11, M01-13, M03-02, M03-09 |
| M03-11 | [Prompt iteration loop](M03-11-prompt-iteration-loop.md) | S | M03-01, M03-02 |

Suggested order: 01 → 04 → 02 → 03 (compressor live: first savings) →
09 → 08 → 07 → 02b → 05 → 06 (disabled by default until M04's ablation)
→ 10 → 11. The id `M03-02b` is kept because the M04 outline references
it. Day-one roles per the decision below: compressor only; the compactor
follows once M03-08's live cache check is in; the selector ships off.

## Interfaces this milestone relies on

- `StepHooks { before_call, on_tool_result }` with `NoopHooks` default
  (M01-08); `CallContext` exposes the pending request, history, workspace
  and trace handles.
- Tool wrapper stores raw output blobs in full (M01-01) — the compressor
  reads them; `tool.result.blob_id` is the `expand` key.
- `mentor.request` payload fields `prompt_version` and (new) `protocol_version`;
  `mentor_calls.apprentice_applied` (M00-06).
- Inference service request API with role, budget and priority (M02-04)
  and `Bypassed` responses (M02-05).
- Baseline system prompt v1 is frozen (M01-09); M03 adds an addendum block
  *after* it so baseline sessions and apprentice sessions share the cached
  core.

## Decisions to make at start

- Which roles run by default on day one (recommendation: compressor only,
  then compactor, then selector — in order of risk). Observer backend is the
  target from day one; if M02-04b is not ready the stateless backend ships
  first and the observer follows in the same milestone.
- What the observer ingests verbatim vs by reference: mentor text and user
  messages verbatim; tool results as the compressed form plus reference (the
  raw blob would flood a small state and is fetchable anyway).
- Latency budgets per role for the reference machine (from M02-07 bench).
- Whether compactor output goes via `role: "system"` messages or a user
  block, after testing cache behaviour live.
- Append-only rule (SPEC §6): every role prompt and the mentor prompt layout
  must be checked for prefix stability — compaction replaces a suffix, system
  text is frozen per session. Verify with the mentor's `cache_read_input_tokens`
  and the local slot's reused-prefix count in the trace.
