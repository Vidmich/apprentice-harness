# M00-07 — Token accounting and cost

Status: todo
Depends on: M00-03, M00-06
Size: S

## Goal

Every mentor call's `usage` is turned into a cost and aggregated per call,
step, agent, session and calendar day, queryable through `stats.tokens` and
`harness stats tokens`. This is the raw signal for the project's primary
metric (remote tokens per successful task), so the numbers must be exact and
auditable back to individual calls.

## Context

SPEC §3.1 Token accounting, §12.2 metrics. Pricing lives in config
(M00-03 `[pricing.<model>]`) because prices change; cost is derived at record
time and stored (`mentor_calls.cost_micros`) so history stays consistent even
if prices are edited later, while a `--reprice` option can recompute.

## Scope

In: cost formula, storing per-call cost, aggregation queries, RPC result
type, CLI output, apprentice-token counters reserved (filled in M03).
Out: budgets/alerts (later), GUI panel (M01-13).

## Design

### Cost formula

```
cost_usd = input_tokens        * pricing.input      / 1e6
         + output_tokens       * pricing.output     / 1e6
         + cache_read_tokens   * pricing.cache_read / 1e6
         + cache_creation_tokens * pricing.cache_write / 1e6
stored as cost_micros = round(cost_usd * 1e6)
```

`input_tokens` from the API already excludes cached tokens (cache fields are
separate), so the four terms are additive. If no pricing entry exists for the
model, cost is `NULL` and the stats result carries `unpriced_calls > 0`.

### Aggregation

`TokenStats` (in `apprentice-api`):

```json
{
  "range": {"since": ts, "until": ts},
  "totals": {"calls": n, "input": n, "output": n, "cache_read": n, "cache_creation": n, "cost_usd": 1.23, "unpriced_calls": 0},
  "by_model": [{"model": "...", ...same fields}],
  "by_day":   [{"day": "2026-09-11", ...}],
  "by_session": [{"session_id", "title", ...}],      // only when not filtered to one session
  "apprentice": {"invocations": 0, "bypassed": 0, "tokens_in": 0, "tokens_out": 0, "estimated_saved_input": 0}   // M03 fills
}
```

Implemented as SQL over `mentor_calls` (indexes exist from M00-06). Days are
computed in the daemon's local timezone, stated in the result (`tz`).

### CLI

```
harness stats tokens [--since 7d|2026-09-01] [--until ...] [--session ID] [--by day|model|session] [--json]
```

Human output: a compact table plus a one-line total, e.g.
`14 calls · in 182,340 · out 21,004 · cache read 610,222 · $1.84`.

### Reprice

`harness stats reprice [--model M] [--since ...]` recomputes `cost_micros`
from current config; logs the number of rows changed. (Rarely needed; keeps
history honest after a pricing correction.)

## Acceptance

- [ ] Unit test: known usage × known pricing → exact micros (including
      rounding, and NULL for an unpriced model).
- [ ] Integration: three recorded calls across two days and two models →
      totals, by_model and by_day sums match hand-computed values.
- [ ] `stats.tokens` filtered by session returns only that session.
- [ ] `harness stats tokens --json` output validates against the
      `TokenStats` type (round-trip through serde).
- [ ] `reprice` updates only the selected rows and is idempotent.

## Verification

`cargo test -p apprentice-core stats::` and a CLI smoke test in M00-09.

## Notes

- Do not estimate tokens locally (no tiktoken-style counting); only API
  `usage` and `count_tokens` results are trusted. Local estimates for the
  apprentice appear in M03 and are labelled as estimates.
- Default cache pricing in config is an assumption (10% read, 125% write of
  input); verify on the pricing page and correct the defaults.
