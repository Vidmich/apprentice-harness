# M02-07 — Load benchmark

Status: todo
Depends on: M02-04, M02-04b, M02-05
Size: M

## Goal

`harness models bench`: a repeatable load generator that drives the
inference service with simulated N-agent traffic whose prompt and output
sizes come from real traces, and reports tokens/s (prefill and decode),
slot utilisation, p50/p95/p99 latency per role, queue waits, bypass rate,
the planner's memory table next to the measured numbers, and the session-
state cost table — as JSON under `<data_dir>/bench/` plus a Markdown
summary. It is the instrument for the M02 exit criterion ("two candidate
base models run concurrently for 4+ simulated agents without exceeding
role latency budgets on the reference machine and on CPU-only") and the
source of the per-machine slot/context defaults.

## Context

SPEC §7 ("Throughput benchmarking command reporting tokens/s, slot
utilisation and p50/p95 latency per role under simulated N-agent load";
"slot count and per-slot context are configured per machine and reported
by the bench"). The M02 README wants the VRAM budget table reported, not
just tokens/s, and the snapshot cost table (M02-04b) to come from here.
M03 needs the per-role budgets it will configure to be grounded in these
numbers.

## Scope

In: the workload model, sampling from traces, the load generator, the
report, storage and listing of runs, comparison of two runs, RPC with
progress, CLI, the exit-criterion check.
Out: task-level evaluation (M04 — this measures the service, not the
roles), GUI pages beyond the "last result" card (M02-08).

## Design

### Workload

A bench run is `BenchSpec`:

```toml
model = "qwen2.5-coder-3b-q4_k_m"     # or two: models = [...] for the concurrent-models case (loaded in turn per phase, or both when VRAM allows — M02 runs them sequentially and reports each; the exit criterion's "concurrently" is satisfied by the two-model phase below)
agents = 4                            # simulated agents, each a loop: think (pause) → request → wait → …
duration_s = 120                      # or requests = N
seed = 42
roles = ["compressor", "compactor", "observer"]   # each with a mix weight and its budget from config
mix = { compressor = 0.6, compactor = 0.1, observer = 0.3 }
prompts = "traces"                    # traces | synthetic
states = true                         # also measure M02-04b: append/snapshot/restore/fork at 4k/16k/32k
warmup_s = 10
```

`prompts = "traces"` samples from the local trace store: compressor
inputs are real `tool.result` blobs (shell, grep, read_file outputs —
the sizes that matter), compactor inputs are `session_messages` windows,
observer appends are the event stream of real sessions; the bench wraps
them in a fixed synthetic role prompt (a placeholder until M03's
protocol exists) so token counts are realistic while outputs are
throw-away. Output lengths are sampled from a per-role distribution
(compressor: 64–512 tokens, compactor: 256–1024, observer: 0 = append
only). `synthetic` uses Lorem-code of the same size distribution when
the store is empty (CI).

Agents run on the async side as `tokio` tasks with a think-time
distribution (exponential, mean 2 s) between requests — the pattern of
one agent per session doing a step at a time. Requests carry the role's
configured budget (M02-05), `Priority::Interactive` for compressor/
selector and `Background` for compactor/observer, and a synthetic
session/agent so their `apprentice.invocation` records are attributable
(`capture_io` off; `bench` sessions are excluded from `stats.tokens`
by default and deleted by `harness models bench --clean`).

### Measurements

From the service's metrics (M02-05, reset at warmup end) and the
records: per role — requests, bypassed (by reason), p50/p95/p99 total
latency, queue wait, prefill ms and tokens/s, decode tokens/s per stream
and aggregate, cached-prefix ratio; global — slot utilisation, aggregate
tokens/s, peak queue depth, preemptions, `memory_used()` (KV cells,
bytes) at peak versus the planner's table, GPU memory free before/after
(profile probe), load time, the two EWMAs' accuracy (`eta_ms` vs actual
start). With `states = true`: the M02-04b cost table (append throughput,
snapshot/restore/fork latency and bytes at 4k/16k/32k, compressed size).

Budget check: for every role, the share of requests that started within
`start_by` and finished within `finish_by`; the run **passes** when the
bypass rate is < 5 % and p95 latency ≤ `finish_by` for every interactive
role (the M02/M03 criteria), else fails with the offending role named.

### Report

`<data_dir>/bench/<utc>-<model>-<agents>a.json` (`BenchReport {spec,
machine: HardwareProfile, plan, results…, pass: bool, reasons}`), a
sibling `.md` with the tables, and `bench/index.jsonl`. `harness models
bench list|show ID|compare A B` (compare prints deltas per role; used
when tuning slots/ctx or after a llama.cpp bump). `harness models bench
--suggest` runs the sweep for the exit criterion: `slots ∈ {2,4,6,8}` ×
`ctx ∈ {8k,16k,32k}` within the planner's fit, picks the largest
configuration that passes with the fewest bypasses, and prints the
`[inference]` lines to paste into config (`--apply` writes them to the
user layer).

### Two-model phase

`models = [A, B]`: the run executes the workload against A, then B
(sequential, same seed), and — when the planner says both fit at the
reduced slot counts — a third phase with A and B loaded together (two
service instances sharing the GPU; `inference.auto_swap` is not used) so
the "two candidate base models run concurrently for 4+ simulated agents"
criterion is measured literally. The report marks which phases ran and
why.

### Surfaces

- RPC `models.bench {spec} → {subscription}` streaming `bench.progress
  {phase, elapsed_s, requests, bypassed, tokens_per_s}` and `bench.done
  {report_id}`; `models.bench_list`, `models.bench_get {id}`.
- CLI `harness models bench [--model ID]... [--agents N] [--duration S]
  [--roles R,..] [--synthetic] [--states] [--suggest [--apply]] [--json]`
  (live line updating on stderr, the summary on stdout), `harness models
  bench list|show|compare|clean`.
- `just bench` runs the reference configuration on the current machine
  and stores the report; the completion notes of this task attach the
  reference machine's and a CI runner's (CPU) reports.

## Acceptance

- [ ] Mock backend with deterministic per-token delays: a 4-agent, 20 s
      run produces a report whose per-role counts equal the number of
      requests the agents issued, whose p95 matches the scripted delays
      within one batch iteration, and whose bypass count equals the
      admission decisions the mock forced; `compare` of two identical
      runs shows zero deltas.
- [ ] `prompts = "traces"` on a store with M01 dogfood sessions samples
      only `tool.result` blobs / message windows of the requested sizes
      and never leaks their content into the report (the report holds
      sizes and ids only).
- [ ] Reference machine, 3B Q4_K_M, 4 agents, defaults: the run passes
      (bypass < 5 %, compressor p95 ≤ 4 s); `--states` fills the cost
      table; `--suggest` prints an `[inference]` block and `--apply`
      writes it; the two-model phase runs 3B + 7B (or 3B + 4B) and its
      report is attached to the completion notes.
- [ ] CPU-only (reference machine with `device = cpu`, and a CI runner):
      the run completes and the report says pass/fail honestly with the
      reasons (a fail on the CI runner is acceptable; a crash is not).
- [ ] Bench sessions are excluded from `harness stats tokens` and
      `session list` by default and removed by `--clean`.
- [ ] `bench.progress` events reach the CLI at ≥ 1 Hz and the run is
      cancellable (CTRL-C ends the agents, unloads nothing, writes a
      partial report marked `aborted`).

## Verification

`cargo test -p apprentice-core bench::` (mock backend, synthetic prompts);
`just bench` on the reference machine with a GPU and with `device = cpu`;
the nightly job runs the CPU bench on the Linux runner and uploads the
report.

## Notes

- The bench measures the service, not the apprentice's usefulness: a
  fast wrong answer passes here and fails in M04. Keep the two separate
  in the report's wording.
- Keep one run under three minutes in the default configuration so it
  gets run after every llama.cpp bump; the sweep is the long one.
