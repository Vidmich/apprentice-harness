# M02-05 — Bypass and back-pressure

Status: todo
Depends on: M02-04
Size: S

## Goal

The service never stalls the coding loop: a request that cannot start
within its budget is rejected immediately as `Bypassed{reason}` instead
of queued, interactive requests go ahead of background ones (and may
pause them), and the service exposes live metrics — queue depth, slot
utilisation, bypass rate and p50/p95 latency per role — through
`inference.status` and the reserved `stats.tokens.apprentice` shape.
Bypass rate becomes a tracked number from here on.

## Context

SPEC §3 ("never stalls the coding loop (bypass to remote on overload)",
"Fail open to the mentor"), §7 ("Latency budgets per role and a bypass
rule: if a request cannot be served within budget, the orchestrator
proceeds without the apprentice for that step and records a bypass.
Bypass rate is a tracked metric"; "priority queue: interactive roles
before background roles"). M00-07 reserved `ApprenticeStats {invocations,
bypassed, tokens_in, tokens_out, estimated_saved_input}`; the M03 exit
criterion is bypass rate < 5 % with no observed stalls.

## Scope

In: admission control with deadlines, the priority queue and background
preemption, bypass reasons and their trace records, metrics collection
and the RPC/CLI/stats surfaces, per-role budget defaults in config.
Out: deciding *which* roles run (M03), the GUI panel that shows the
metrics (M02-08), the load generator (M02-07).

## Design

### Budgets

`Budget { start_by: Option<Duration>, finish_by: Option<Duration> }` on
every request (M02-04). Defaults per role from config, overridable per
request by the caller:

```toml
[inference.budgets]                # milliseconds; the orchestrator (M03) reads these too
default    = { start_by = 1500, finish_by = 8000 }
compressor = { start_by = 800,  finish_by = 4000 }     # on the critical path between tool result and mentor call
compactor  = { start_by = 2000, finish_by = 15000 }
selector   = { start_by = 800,  finish_by = 3000 }
observer   = { start_by = 3000, finish_by = 0 }        # appends: 0 = no finish deadline
bench      = { start_by = 0,    finish_by = 0 }
```

### Admission (in the scheduler, at `Submit`)

A request is admitted only if the scheduler expects it to *start* prefill
within `start_by`:

```
eta_start = 0 if a slot is free (or a background slot can be paused for an Interactive request)
          else min over slots of (remaining_generation_tokens × decode_ms_per_token_ewma)
          + queued_ahead_prefill_tokens × prefill_ms_per_token_ewma / n_parallel
admit iff eta_start <= start_by   (start_by = 0 → always admit)
```

The two EWMAs are measured by the scheduler per batch (prefill tokens/s,
decode tokens/s at the current slot occupancy) and reported in status.
Not admitted → `InferenceError::Bypassed{reason: Deadline{eta_ms}}`
returned at once — no queueing. Other immediate reasons: `ModelLoading`
(unless `start_by` covers the expected remaining load time, tracked from
the last load), `NoModel`, `Disabled`, `QueueFull` (`inference.max_queue`,
default 64), `PromptTooLong`, `Unavailable` (backend error state after N
consecutive decode failures; the service reloads in the background).

`finish_by`: a generating request past its finish deadline is stopped
with `stop: Budget` and *what it produced so far* is returned as a normal
response with `usage.truncated_by_budget = true` — the caller (M03)
decides whether a partial compression is usable; the record says it was
cut.

### Priority and preemption

Two queues. `Interactive` requests are assigned slots before any
`Background` request. When no slot is free and an interactive request
arrives within budget, the scheduler **pauses** the youngest background
generation: its sequence keeps its KV (the slot is marked `Paused` with
its request), the interactive request takes a *different* free sequence
if one exists — otherwise (all sequences busy) the paused request's
progress is snapshotted with `state_seq_save` into memory, its sequence
is cleared, and it is resumed later by `state_seq_load` (or, past
`inference.pause_max_s`, failed with `Bypassed{Preempted}`). Resident
states (M02-04b) are never preempted. `inference.background_slots`
(default `slots - 1`) caps how many slots background work may hold, so
one slot is always free for interactive requests without any pausing in
the common case.

### Records and metrics

- Every bypass writes `apprentice.invocation {role, bypassed: true,
  reason, eta_ms?, queue_depth, slots_busy, latency_ms: 0, tokens_in: 0,
  tokens_out: 0}` (M02-04's record with `bypassed: true`), with the
  session/agent/step when given, so the bypass rate is a query over the
  trace, not only an in-memory counter.
- `Metrics` (in the service, reset on load): per role — requests,
  bypassed by reason, completed, cut by budget, latency histogram
  (fixed log buckets 1 ms … 60 s; p50/p95/p99 derived), queue wait
  histogram, tokens in/out; global — queue depth (now, max), slot
  utilisation (busy-slot-seconds / wall-seconds since load), prefill and
  decode tokens/s EWMA, preemptions, pauses resumed/failed.
- `inference.status` returns them; `stats.tokens` fills
  `apprentice {invocations, bypassed, tokens_in, tokens_out,
  estimated_saved_input: 0}` from `apprentice.invocation` events in the
  range (grouped like the mentor calls when `group_by` asks; a new
  `by_role` table), so the CLI's `harness stats tokens` shows the local
  side next to the remote side. `harness stats apprentice [--since]
  [--by role|day]` prints requests, bypass rate, p50/p95, tokens.

### Surfaces

- `inference.status` (extended), `inference.metrics_reset` (bench uses
  it), `harness apprentice status` (one screen: model, slots, queue,
  bypass rate per role, p50/p95, tokens/s).
- Config: `[inference.budgets]`, `max_queue`, `background_slots`,
  `pause_max_s = 30`, `failure_reload_after = 3`.

## Acceptance

- [ ] Mock backend with a scripted per-token delay: 4 slots busy with
      1 s of decode left, a compressor request with `start_by = 800 ms`
      is bypassed at once with `Deadline{eta_ms ≈ 1000}` and the trace
      record is written; with `start_by = 1500` it is admitted and starts
      when a slot frees; `start_by = 0` always admits.
- [ ] `finish_by` cuts a generation at the deadline (±1 iteration) and
      returns the partial text with `truncated_by_budget`.
- [ ] Priority: 4 background generations occupy `background_slots = 3`
      plus one queued; an interactive request takes the free slot
      immediately; with `background_slots = 4` it pauses the youngest
      background one, which resumes after and produces the same total
      output as uninterrupted (mock is deterministic).
- [ ] `ModelLoading` bypass while the mock "loads" for 2 s, unless
      `start_by` ≥ the remaining load estimate; three scripted decode
      failures flip the service to `Unavailable`, reload, and it serves
      again.
- [ ] `stats.tokens` over a range with 10 invocations of which 2 bypassed
      reports `apprentice.invocations = 10, bypassed = 2` and the
      `by_role` table; `harness stats apprentice` prints the p50/p95 that
      the metrics reported.
- [ ] Under M02-07's simulated 4-agent load on the reference machine,
      no request with the default budgets waits in the queue longer than
      its `start_by` (the bench asserts it) — the "no stalls" half of the
      M03 exit criterion, measured before M03 exists.

## Verification

`cargo test -p apprentice-core inference::admission::` and
`inference::metrics::` with the mock backend; the load run is M02-07's.

## Notes

- Immediate rejection is the point: a queued request the orchestrator
  waits on *is* the stall SPEC forbids. If in doubt, bypass.
- The ETA estimate is deliberately simple; the bench will show how far
  off it is (`eta_ms` vs. actual start is in the record) and M03 can
  tune the budgets from real numbers.
