# M3 — Apprentice v0: prompted roles and protocol v1

Outline only. Full task files are written when the milestone starts. Goal
and exit criterion: `../../ROADMAP.md`. Spec: `SPEC.md` §4 (hook points),
§5 (roles), §6 (mentor–apprentice protocol).

## Planned tasks

| Id | Title | One-line goal |
|---|---|---|
| M03-01 | Protocol v1 artifact | `protocol/v1/`: mentor system-prompt *addendum* (separate from the M01-09 baseline so A/B stays possible), apprentice role prompts, wire-format spec (`#ctx`, `#result`, `#state`, `#need`, `expand <id>`, `#ask-apprentice`), CHANGELOG; `protocol_version` recorded in every `mentor.request` payload. |
| M03-02 | Apprentice orchestrator | Implements the `StepHooks` trait from M01-08: decides per step which roles run, enforces latency budgets via the inference service (M02-04/05), merges outputs into the request, records `apprentice.invocation` events with token deltas and bypass reasons; `--no-apprentice` / config toggle. |
| M03-03 | Role: output compressor | Prompted compression of tool results (shell, grep, read_file, diff) with type-specific prompts; keeps exact strings/line numbers; raw blob stays retrievable by id; replaces the M01-01 head/tail truncation when enabled. |
| M03-04 | `expand` by reference | Mentor tool `expand {result_id, range?}` returning the raw result (or a slice) from the trace blob; counts as a "regret" signal for the compressor when used. |
| M03-05 | Repository index | Tree-sitter based symbol index (functions, types, imports) and chunking per file; incremental refresh from the workspace index (M01-02); query API used by the selector and later by the ranker (M06). |
| M03-06 | Role: context selector | Prompted selection: given task + state + candidate chunks from the index, produce a ranked, budgeted `#ctx` block injected before the mentor call; ablation-friendly output (chunk ids kept in the trace). |
| M03-07 | Role: history compactor | Prompted state document (`#state`) replacing older turns when history exceeds a threshold; keeps thinking-block and tool_use/tool_result integrity for the API; comparison hook against Anthropic server-side compaction for M04. |
| M03-08 | Mid-conversation state injection | Use the API's mid-conversation `role: "system"` messages (Opus 5 supports them) for per-turn `#state`/`#ctx` so the cached prefix is never invalidated; fallback to a user-turn block on models without support. |
| M03-09 | Local token estimation | Apprentice-side token counts (llama tokenizer) and estimated *saved* mentor input tokens per invocation (baseline size − sent size), labelled as estimates; feeds `stats.apprentice`. |
| M03-10 | GUI: apprentice contribution view | Per step: which roles ran, latency, bypassed?, tokens before/after, expand-to-see the compressed vs raw result; session and day totals of estimated savings next to the cost panel (M01-13). |
| M03-11 | Prompt iteration loop | `harness protocol diff\|bump`; every prompt change requires a CHANGELOG entry; once M04 exists, a replay evaluation run is attached to the entry. |

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
  then compactor, then selector — in order of risk).
- Latency budgets per role for the reference machine (from M02-07 bench).
- Whether compactor output goes via `role: "system"` messages or a user
  block, after testing cache behaviour live.
