# M03-04 — `expand` by reference

Status: todo
Depends on: M01-01, M03-01
Size: S

## Goal

A mentor tool `expand {result_id, range?, pattern?}` that returns the
raw tool result — or a line range / the lines matching a pattern — from
the trace blob, so a compressed or truncated result is lossless on
demand. Every use against a compressed result is recorded as a
**regret** signal for the compressor (the mentor needed what the
apprentice left out); uses against a merely truncated result (baseline
sessions) are recorded too, as the M01-01 policy's own regret.

## Context

SPEC §5.1 ("must be *lossless on demand* — the raw result stays available
and the mentor can request it in full by reference (`expand
<result-id>`)"), §6 ("the mentor can always ask for the raw data"), §10
(regret-type signals feed training). M01-01 stores raw blobs whole (up
to `tools.max_capture_bytes`, then head + tail) and the truncation
marker already names the blob; `Executed.blob_id`, `BlobFiles::read`,
`tool.result.blob_id` in the trace. M03-01's addendum tells the mentor
about the tool and the `expand blob:<id> …` trailer M03-03 appends.

## Scope

In: the tool (schema, registry entry, permission class, output limits),
blob ownership checks, range and pattern slicing, the `apprentice.expand`
record and its use in stats, availability in baseline sessions, tests.
Out: expanding anything that is not a tool result (chunks from the index
are read with `read_file`; `#ctx` entries carry their path and range),
GUI display (M03-10 shows regrets per step).

## Design

### Tool (`tools/expand.rs`)

```json
{ "name": "expand",
  "description": "Return the raw output of an earlier tool result by its blob id (from a #result block or a truncation marker), optionally a line range or only the lines matching a regex with context.",
  "input_schema": { "result_id": "string (blob:<id> or <id>)", "range": { "start": int, "end": int }?, "pattern": "string (regex)?", "context": int? (default 2), "max_bytes": int? } }
```

Registered in the standard tool set for **every** session (baseline
sessions have truncated results with markers; the tool is useful and
its `prompt_version`/tools hash bump is a one-time prefix change at
the M03 release). Permission class: read-only, allowed by default
(M01-07 rules can still deny it). Ownership: the blob must be
referenced by a `tool.result` event of the **same session** (or of a
parent session for M07's sub-agents); anything else → `InvalidInput`
"unknown result id" — blobs are content-addressed and shared across
sessions, so the check is on the event, not the blob.

Output: the raw text (UTF-8; binary → `InvalidInput`), line-numbered
`L<n>│` like the compressor's input, cut to `max_bytes` (default
`tools.max_mentor_bytes`) with M01-01's head/tail marker; `range` slices
lines (1-based inclusive; clamped, out-of-range → error naming the line
count); `pattern` returns matching lines with `context` lines around,
merged when they overlap, up to `max_bytes`. The result header line:
`blob:<id> lines=<n> bytes=<n> [range L<a>-<b> | matches=<n>]`.

### Regret record

Every `expand` call writes, besides its own `tool.call`/`tool.result`,
an event `apprentice.expand {result_id, target_event_id, target_step_id,
compressed: bool, compressor_invocation_id?, range?, pattern?, bytes_
returned, bytes_in_target_block}` — `compressed` is whether the target's
`tool.result.compressed` is true (M03-03), `false` for a baseline
truncation. `stats.tokens.apprentice` gains `regrets` (count of expands
on compressed results) and `harness stats apprentice` prints the regret
rate (regrets / compressor invocations) per role and per tool type — the
first cheap proxy for over-compression before M04's replay evaluator.
M05 turns the record into a feedback signal for training; the shape is
fixed here.

### Surfaces

- The tool itself; `harness trace show <session> --expands` lists the
  regrets with their targets.
- `harness tools call expand --result-id blob:… --range 40:60` for
  manual poking (M01-01's tool CLI).

## Acceptance

- [ ] Schema snapshot (`tools::schema` goldens) includes `expand`; the
      tools hash changes once and `session.prefix_changed` fires for a
      resumed pre-M03 session (M01-08's mechanism).
- [ ] Whole result, `range`, `pattern` with context (overlapping
      contexts merged), `max_bytes` cut with the marker; a range past
      the end → error with the line count; a binary blob → error.
- [ ] Ownership: a blob id from another session → `InvalidInput`; the
      same bytes referenced by this session's own result → ok.
- [ ] `apprentice.expand` is written with `compressed: true` for a
      compressed target and `false` for a truncated one; `stats.tokens`
      counts only the former as `regrets`; `harness stats apprentice`
      prints the rate.
- [ ] Baseline session (`--no-apprentice`): a truncated 200 KB result's
      marker names the blob and `expand` returns the omitted middle by
      range.
- [ ] Live: in a dogfood session the mentor, given the addendum, calls
      `expand` on a compressed cargo error result and continues
      correctly (observed once, transcript id in the notes).

## Verification

`cargo test -p apprentice-core tools::expand::` (store with fixture
blobs; permission rules); the golden update; the live observation.

## Notes

- `expand` is deliberately the *only* way the mentor gets raw bytes back
  from the apprentice channel; `#need raw` (M03-03) is apprentice →
  harness, never mentor-facing. The addendum says so.
- Keep line numbering identical between the compressor's input, `ref`
  locators and `expand` output, or locators will be off by one in one
  of the three places.
