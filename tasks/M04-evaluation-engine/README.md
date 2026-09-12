# M4 — Evaluation engine

Outline only. Full task files are written when the milestone starts. Goal
and exit criterion: `../../ROADMAP.md`. Spec: `SPEC.md` §12.

## Planned tasks

| Id | Title | One-line goal |
|---|---|---|
| M04-01 | Task corpus format | `corpus/<name>/<task>/task.toml` (repo source: git URL+commit or bundled tarball, setup commands, prompt, success check: test command / file assertions / judge rubric), plus a *private* corpus generated from own traces (`harness corpus add-from-session ID`) with the start snapshot as the repo state. |
| M04-02 | Public corpus importers | Importers for SWE-bench-Lite/Verified subsets and Aider polyglot into the corpus format; pinned commits; licence notes; sandboxed setup (containers where available, plain temp dirs otherwise). |
| M04-03 | Headless task runner | Run one task: materialise repo in a temp workspace, run the agent with a given *configuration* (mentor model, effort, apprentice on/off, protocol version, adapters), apply the success check, record everything under an `eval_runs` table linked to the trace; permission mode `auto` with a hard deny-list. |
| M04-04 | Replay evaluator | For logged steps: substitute the apprentice output (compressed result / selected context / compacted history) into the stored request, re-ask the mentor for that single step, compare with the original action — exact match for tool calls and edits, LLM judge (cheap model) for text; per-role sufficiency rate and token delta; runs in minutes; `harness eval replay`. Observer steps are re-run from the `apprentice.state` snapshot recorded at that step (M02-04b/M03-02b), so the apprentice side is exact, not re-ingested. Adds a **precise-reference recall** metric (blob/line/identifier pointed at correctly) per role. |
| M04-05 | A/B bench runner | `harness bench --corpus X --config A --config B --seeds N`: runs configurations over the corpus, collects success, remote tokens (in/out/cached), cost, round trips, wall-clock, bypass rate, regret (expand calls, retries after apprentice decisions); bootstrap confidence intervals; net-savings accounting. |
| M04-06 | Reports and regression gate | Report format (JSON + Markdown + GUI page): baseline vs candidate, deltas with CIs; gate rule (sufficiency ≥ threshold, success within tolerance, net tokens improved) as a single pass/fail with reasons; `harness eval gate` used by M03-11 and by M06 promotion. |
| M04-07 | Ablation labelling | Chunk/result ablation on logged steps (remove item → does the mentor's action change?) producing relevance labels stored as `eval.label` events — the supervised signal for the selector and compressor datasets in M05/M06; uses the Batch API and prompt caching to keep cost down. |
| M04-08 | Judge | LLM-judge harness with fixed rubrics and calibration set; judge model configurable (`claude-haiku-4-5` default), judge calls recorded and priced like other mentor calls (`mentor_calls.kind = "judge"`). |
| M04-09 | Base model selection run | Using M02 models and M03 prompted roles, run replay + bench across candidate base models/quants, **including at least one recurrent/hybrid model** and both role backends (observer vs stateless) per model; report sufficiency, precise-reference recall, local cost per step and state size; produces the report that fixes the default base model and architecture (SPEC §17 deferred decision). |
| M04-10 | GUI eval dashboard | Corpus list, run history, report viewer with per-task drill-down into traces, gate status per protocol/adapter version. |

## Interfaces this milestone relies on

- Replay guarantee and `trace replay-check --rebuild` (M01-14): the
  evaluator must be able to reconstruct any request and substitute at the
  hook points (M01-08 / M03-02).
- Outcome events (`tests`, `files_changed`, `user_accept`) (M01-15) for
  labelling private-corpus tasks.
- Workspace snapshots with `git_head` + dirty diff (M01-06) to materialise
  private tasks at their start state.
- Token accounting per call with `kind` (M00-07, M01-13) so eval and judge
  spend is separable from real usage.
- `apprentice.invocation` events with chunk/result ids (M03) for ablation.
- Headless daemon behaviour and permission `auto`/headless rules (M01-07,
  M00-08).

## Decisions to make at start

- Success-rate tolerance and sufficiency threshold for the gate (proposal:
  success within −2 points absolute at 95% CI, sufficiency ≥ 95%).
- Sandboxing approach for public corpus tasks (Docker/Podman if present).
- API budget for ablation labelling (deferred decision; M04-07 is the first
  task that spends meaningfully).
