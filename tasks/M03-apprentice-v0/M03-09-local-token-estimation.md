# M03-09 — Local token estimation

Status: todo
Depends on: M03-02, M02-04, M01-13
Size: S

## Goal

Honest, labelled numbers for what the apprentice saved: per invocation,
the mentor-token size of what *would* have been sent (the baseline —
M01-01's truncated result, the cut turns, nothing for the selector)
minus what *was* sent, in an estimate of the mentor's tokeniser
calibrated per session against the API's own counts; per session and
per day, totals in `stats.tokens.apprentice.estimated_saved_input` next
to the mentor's real usage. Apprentice-side counts (its own tokenizer,
exact) are separate from mentor-side estimates and both say which they
are.

## Context

SPEC §12.2 (token savings as a primary metric), §13/§14 (cost panel and
`stats` show savings). `ApprenticeStats { invocations, bypassed,
tokens_in, tokens_out, estimated_saved_input }` reserved since M00-07;
`stats.tokens` and `harness stats tokens` (M01-13) show mentor usage per
model/day/session; `InferenceService::tokenize` (M02-04) is the local
tokenizer; `Mentor::count_tokens` (M01-08) asks the API for an exact
count of a request; M03-02's record has `tokens_before/after` slots.

## Scope

In: the estimator, calibration, per-invocation fields, per-role
semantics of "saved", the stats aggregation and CLI/RPC shapes, the
labels, tests.
Out: cost in USD of the savings (M04's reports multiply by the price
table — the estimate is in tokens only), the GUI (M03-10), replay-
measured savings (M04, which replaces estimates with measured deltas
where it can).

## Design

### Two counters, two labels

- `local`: exact counts from the apprentice's tokenizer — the
  invocation's `tokens_in/cached/out` (M02-04's `Usage`). Already exact.
- `mentor_est`: estimated mentor tokens of a text = `bytes ×
  ratio_session` where `ratio_session` is calibrated (below); shown
  everywhere with the suffix `≈` and the field name `estimated_*`. The
  mentor's real usage is never mixed with estimates in one number.

### Calibration (`TokenEstimator`, per session, in `Conversation`)

Every mentor response's `usage.input_tokens + cache_read + cache_
creation` against the request's byte size gives one sample; the
estimator keeps an EWMA of tokens/byte over the session (seed: the
last 50 calls of the same model from `mentor_calls` at session start;
cold default 0.28 for Claude models on code+English). Text classes
differ (a diff tokenises worse than prose), so the estimator keeps one
ratio per class — `code`, `log`, `prose`, `json` — classified by a
cheap heuristic, with per-class seeds and one `count_tokens` call per
session per class the first time a class appears (a `CallKind::Count`
call, already recorded by M01-08's guard path) to anchor the ratio.
`estimate(text, class) -> u32`; `confidence` = samples seen.

### Per-role semantics

| Role | `tokens_before` (baseline) | `tokens_after` (sent) | `estimated_saved` |
|---|---|---|---|
| compressor | the M01-01 truncated block (not the raw blob) | the final `#result` block incl. expansions | before − after |
| compactor | the cut turns as they were | the `#state` message | before − after, counted once at the cut (the saving *recurs* every later call; `recurring: true` and M04 sums it properly) |
| selector | 0 | the `#ctx` block | −after (an *added* cost; `estimated_added`) |
| ask | 0 | the answer block | 0 (a substitute for a remote round trip; M04 counts calls avoided) |
| observer ingest | — | — | none (no mentor-facing output) |

Bypassed invocations: `estimated_saved = 0`. All three fields are in the
`apprentice.invocation` record; `estimator: {ratio, class, confidence}`
alongside.

### Aggregation

`stats.tokens.apprentice` becomes `{invocations, bypassed, tokens_in,
tokens_out /* local, exact */, estimated_saved_input, estimated_added_
input, net_estimated: saved − added, regrets /* M03-04 */, by_role:
[{role, invocations, bypassed, p50_ms, p95_ms, tokens_in, tokens_out,
estimated_saved, estimated_added}]}` computed over `apprentice.
invocation` events in the range (grouped like mentor calls when
`group_by` asks: by day/session/workspace); `session.get` gains the same
object for the session. `harness stats tokens` prints an "apprentice"
line with `≈` and `harness stats apprentice` the by-role table; `--json`
carries the labels as field names.

### Config

`apprentice.estimator.seed_ratio = 0.28`, `calibrate_with_api = true`
(off: heuristics only, for tests and offline), `classes = true`.

## Acceptance

- [ ] Estimator unit: fed 20 (bytes, tokens) samples per class it
      converges within 5 % of the true ratio; a `count_tokens` anchor
      moves the ratio at once; cold start uses the seed and reports
      `confidence: 0`.
- [ ] Compressor record on the mock loop: `tokens_before` is the
      truncated baseline's estimate, not the raw blob's; a 200 KB raw
      result truncated to 32 KB and compressed to 2 KB reports the
      32 KB-based saving.
- [ ] Compactor record has `recurring: true`; selector record has
      `estimated_added > 0` and `estimated_saved = 0`; bypassed records
      report 0.
- [ ] `stats.tokens` over a fixture range sums saved/added/net and the
      `by_role` table; `harness stats tokens` prints `apprentice ≈ …`;
      the Rust snapshot and the GUI `api.ts` golden agree.
- [ ] `calibrate_with_api = false` never calls `count_tokens` (wiremock
      asserts no call); `true` makes at most one per class per session.
- [ ] Live: over a 20-step dogfood session the session's estimated
      total is within 15 % of the difference between the mentor's
      actual input tokens and a replayed baseline of three steps
      (computed by hand with `count_tokens` on the baseline requests;
      the procedure and numbers in the completion notes).

## Verification

`cargo test -p apprentice-core stats::apprentice:: runtime::apprentice::
estimate::`; the live spot-check.

## Notes

- Estimates are for the dashboard; decisions (which prompt, which
  model) wait for M04's measured deltas. Keep the `estimated_` prefix
  on every field so nobody quotes them as measurements.
- The compressor's baseline is the *truncated* text on purpose: the
  harness never sent raw 200 KB results before M03 either.
