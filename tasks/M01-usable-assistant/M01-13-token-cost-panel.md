# M01-13 — Token and cost panel

Status: done
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

- [x] Session totals match `stats.tokens --session` exactly after a run.
      — one source: `agent.usage` carries the conversation's running
      totals, seeded from the store's sums for the session
      (`runtime/mod.rs::seed_totals`, now with the call count), and
      `session.get` / `session.list` sum the same rows
      (`core/tests/sessions.rs`: `calls == 3` after the run, the
      conversation at 4 after a resume; `loop.rs`: `session_calls`
      1, 2, 3). Checked against the mock: the header and the panel's
      session row both said `4 · 5,680 · 586 · $0.0426`.
- [x] Usage panel ranges and groupings match CLI output for the same
      range. — the panel sends the range the CLI takes (`today` → the
      local date, `7d` / `30d` ages, custom dates) and asks
      `stats.tokens` for every `group_by`; the tables are the CLI's
      columns (`core/tests/stats.rs` for the sums, `cli/tests/stats.rs`
      for the flags). The real-app comparison is in `TESTING.md`.
- [x] Title calls appear as kind `title` and are included in totals but
      distinguishable. — `by_kind` (`stats.tokens`), the `kind` column
      of `stats.calls`, the kind table and a badge in the calls table;
      `group_by_picks_the_breakdowns_and_adds_workspace_and_kind`
      counts the title call in the totals and apart.
- [x] "View request" shows the exact stored body; CSV export opens in a
      spreadsheet with correct columns. — the drawer reads
      `trace.get {request_event_id, include_blob}` and shows the blob
      (indented when it parses as JSON, as stored otherwise); the CSV
      has the 18 columns of `harness stats calls --csv`, RFC 4180
      quoting, tested on both sides (`cli/src/stats.rs`,
      `lib/usage.test.ts`). Checked against the mock (24 rows saved).

## Verification

Tests for the new RPCs; manual GUI check after a dogfooding day.

## Notes

- Keep the chart component simple (a small SVG bar chart), no heavy
  charting dependency.

## Completion notes (2026-09-13)

`pnpm test` (38 vitest tests: `usage.test.ts` new, `format.test.ts`,
`transcript.test.ts`, `api.test.ts` extended), `pnpm typecheck`,
`pnpm lint`; `cargo test -p apprentice-core` (stats, sessions, loop),
`-p apprentice-api --test snapshots` (goldens updated),
`-p harness` (unit and `tests/stats.rs`).

- RPC: `stats.tokens` takes `group_by: [day|model|session|workspace|kind]`
  (empty = the M00-07 tables) and `workspace_id`; `TokenStats` gains
  `by_workspace` (key = workspace id, `""` for sessions without one,
  label = the root) and `by_kind`. `stats.calls {since, until,
  session_id?, workspace_id?, limit (100, max 1000), offset}` returns
  `{calls: CallSummary[], total}` newest first — the `mentor_calls` row
  joined to its session (`session_title`, `workspace_id`), with
  `agent_id`, `status` and `request_event_id` so a row can open the
  session at its turn and fetch the body. `SessionSummary.calls` (every
  kind) and `agent.usage.session_calls` feed the header's count.
- Store: `CallFilter.workspace_id`, `GroupBy::{Workspace, Kind}`,
  `list_calls_page`; the conversation counts its calls (seeded on
  resume). No migration: `mentor_calls.kind` exists since v003.
- CLI: `harness stats calls [--since --until --session --workspace
  --limit --offset] [--csv | --json]` (a table with a count line
  otherwise); `stats tokens --by workspace | kind`, `--workspace ID`.
  `--json` keeps the M00-07 shape (the three tables); a human gets the
  tables asked for.
- GUI: the session header's `in 182k · out 21k · cached 610k · $1.84 ·
  14 calls` (exact numbers on hover; the cost is left out while a call
  is unpriced) from `agent.usage`, re-read after the title call. The
  Usage view (`$` in the sidebar, `Ctrl+U`): range presets and custom
  dates, the selected workspace as the scope, five tiles, an SVG bar
  chart by day (cost / tokens, gaps drawn empty, tips on hover), tables
  by model, kind, workspace and session (a session row opens it), the
  calls table (50 a page, a session link that opens the session
  scrolled to the call's agent — `stores/jump.ts`, older pages loaded
  on the way, the turn flashed — and "View request"), a drawer with
  the pretty body (copy, save, Esc), "Export CSV" of the whole range
  through the save dialog. The panel re-reads on show, on a range or
  workspace change, after a run and after its title call.
- Mock: every call of a run is recorded with its `mentor.request` body
  in the trace and listed by `stats.calls`; `stats.tokens` groups and
  filters like the daemon (local days, the range grammar); the title
  is a call on the cheap model; a fresh mock seeds ten days of history
  on `C:/src/demo` (two sessions, two models, an error, an unpriced
  model).

Deviations / decisions:

- `mentor_calls.kind` keeps its M01-10 values `step | title` (no
  migration v004; `count_tokens` / `other` have no producer yet — add
  them with the producer).
- `stats.tokens` without `group_by` keeps the M00-07 result so the CLI's
  `--json` and older clients are unchanged; the new tables are only
  filled when asked for.
- "Open the session at that turn" scrolls to the call's agent (its
  first item) rather than the exact call: the transcript is keyed by
  agent and message, not by mentor call.
- Rows per page (50) and the export cap (10 000 rows in pages of 1000)
  are constants in `lib/usage.ts`; the CSV has the CLI's 18 columns in
  the CLI's order so the two exports are the same file.
- The chart is a plain SVG (no charting dependency, per the task note);
  the tokens mode counts input, output, cache read and cache write.
