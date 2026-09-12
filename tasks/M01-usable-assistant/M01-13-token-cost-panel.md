# M01-13 — Token and cost panel

Status: todo
Depends on: M00-07, M01-11
Size: S

## Goal

Visibility of what the mentor costs, at every level: per turn (already in
the transcript footer), per session (header), per day and per workspace
(panel), with a breakdown by model and by kind of call (agent vs title),
and the ability to see the individual calls behind any number. This is the
baseline instrument the apprentice's savings will later be shown against.

## Context

SPEC §13 (token/cost panel per session and per day), §12.2 (metrics). Data
is `stats.tokens` (M00-07) plus a new `stats.calls` list.

## Scope

In: session header totals, Usage panel (day/week/month, by model, by
workspace, by session), calls table with drill-down to the trace event,
CSV export, CLI parity for `stats calls`.
Out: budgets/alerts, apprentice savings columns (M03 adds them here).

## Design

### RPC additions

- `stats.tokens` (existing) gains `group_by: ["day"|"model"|"session"|"workspace"|"kind"]`.
- `stats.calls {since, until, session_id?, workspace_id?, limit, offset}` →
  rows from `mentor_calls` joined to sessions:
  `{call_id, started_at, session_id, session_title, kind, model, effort,
  stop_reason, input, output, cache_read, cache_creation, cost_usd,
  total_ms, request_event_id}`.
- `mentor_calls.kind` column added (migration v004): `agent | title |
  count_tokens | other`.

### GUI

- Session header: `in 182k · out 21k · cached 610k · $1.84 · 14 calls`
  updating live from `agent.usage` (running totals included in the event,
  so no refetch per delta).
- Usage panel (sidebar entry "Usage"): range picker (today, 7d, 30d,
  custom), stat tiles (calls, input, output, cache read, cost), a bar chart
  by day (cost; toggle to tokens), tables by model and by workspace, and
  the calls table with a link that opens the session at that turn and a
  "View request" that shows the `mentor.request` blob (pretty JSON) in a
  drawer.
- "Export CSV" of the calls table (Tauri save dialog).

### CLI

`harness stats calls [--since] [--session] [--json|--csv]`.

## Acceptance

- [ ] Session totals match `stats.tokens --session` exactly after a run.
- [ ] Usage panel ranges and groupings match CLI output for the same
      range.
- [ ] Title calls appear as kind `title` and are included in totals but
      distinguishable.
- [ ] "View request" shows the exact stored body; CSV export opens in a
      spreadsheet with correct columns.

## Verification

Tests for the new RPCs; manual GUI check after a dogfooding day.

## Notes

- Keep the chart component simple (a small SVG bar chart), no heavy
  charting dependency.
