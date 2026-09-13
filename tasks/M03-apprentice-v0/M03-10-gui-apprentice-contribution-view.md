# M03-10 — GUI: apprentice contribution view

Status: todo
Depends on: M01-11, M01-13, M03-02, M03-09
Size: M

## Goal

The user can see what the apprentice did: under every step of the
transcript, a strip naming the roles that ran (or were bypassed, and
why), their latency and backend, and the tokens before → after; a
compressed tool result can be flipped to its raw form side by side;
injected `#state`/`#ctx` blocks render as apprentice items, not user
text; compactions show as a divider. Session totals sit in the chat
header, day and range totals as an "Apprentice" card in the usage panel
next to the mentor's cost (M01-13), all labelled `≈` where estimated.
A toggle in the composer runs the next turn with or without the
apprentice, and the status bar shows the backend and the bypass dot.

## Context

SPEC §13 ("apprentice contribution view: what was done, tokens saved,
bypasses"), ROADMAP M3 exit ("the GUI shows per-step token deltas").
M01-11's transcript (`components/chat/{Items, ToolCard, RawView,
TranscriptView}`, `lib/transcript.ts`), M01-13's usage panel
(`components/usage/`, `stores/usage.ts`, `stats.tokens` with the
reserved `apprentice` object), M01-12's conventions (Zustand stores,
mock daemon, `TESTING.md`), M02-08's status-bar bypass dot. M03-02's
`agent.apprentice` event and `apprentice.invocation` records; M03-04's
`expand`/regrets; M03-07's `agent.compacted`; M03-08's `origin:
apprentice` rows; M03-09's fields.

## Scope

In: the step strip, the compressed/raw flip, apprentice items in the
transcript, the compaction divider, session and range totals, the
composer toggle, the status-bar indicator, the RPCs and events needed,
the mock daemon, Vitest for the pure parts, `TESTING.md`.
Out: the Models view (M02-08), eval dashboards (M04-10), feedback
controls on apprentice output (M05 adds thumbs/verdict UI to this
strip).

## Design

### Data

- Live: `agent.apprentice {session_id, agent_id, step, invocation:
  {role, hook, backend, bypassed, reason, latency_ms, tokens_before,
  tokens_after, estimated_saved, estimated_added, target}}` per
  invocation (M03-02 emits it after the record is written) and
  `agent.compacted` (M03-07).
- History: `session.apprentice {session_id, since_step?}` → `{steps:
  [{step, invocations: [...], compacted?: {...}}], totals: {…the
  M03-09 session object…}}`; `session.get` already carries the totals.
- Raw vs compressed: `trace.blob {id}` (M01-14's read RPC) for the raw
  result; the compressed text is the `tool_result` block the transcript
  already has; `tool.result.compressor` (M03-03) gives refs/expanded/
  dropped counts.
- Range: `stats.tokens.apprentice` (M03-09) via the existing usage
  refresh.

### Transcript (`components/chat/`)

- `ApprenticeStrip` under each step's tool cards: one chip per
  invocation — `compressor · observer · 310 ms · 8.1k → 1.2k ≈`,
  `selector · bypassed (deadline 1.4 s)`, `ask · 90 ms`; hover shows the
  record; a chip with regrets (an `expand` hit this result) is marked.
  Collapsed to one line per step by default; a session-level toggle
  expands all.
- `ToolCard` gains a "raw ⇄ compressed" flip when `compressor` is set:
  the raw view is M01-11's `RawView` over the blob, the compressed view
  renders the `#result` block with locators as links that jump to the
  raw view's range (`lib/wire.ts` parses the block — a TS port of the
  grammar's read side, with goldens shared with the Rust tests).
- Injected messages (`origin: apprentice`) render as `ApprenticeItem`
  (collapsed `#state v3` / `#ctx 6 chunks` headers, expandable), never
  as user bubbles; `agent.compacted` renders as a divider "history
  compacted at step 14 — 61k → 1.4k ≈ tokens".
- Chat header: `apprentice: 12 invocations · 1 bypass · ≈ 38k saved`.

### Usage panel (`components/usage/`)

`ApprenticeCard`: invocations, bypass rate, regrets, estimated saved /
added / net (`≈`), a `by_role` table (invocations, bypassed, p50/p95,
saved) and a bar of estimated saved per day under the existing chart
(same range and workspace scope). "Estimates — see M04 reports for
measured deltas" as the card's footnote.

### Controls

- Composer: an "apprentice" switch (default from `apprentice.enabled`
  via `config.get`), sets `RunOptions.apprentice` for the next `agent.
  run`; a session whose runs mixed both shows a note in the header.
- Status bar: `apprentice · observer` / `stateless (demoted)` / `off`,
  with M02-08's bypass dot; click opens the Models view's service panel.
- Settings → Apprentice: `enabled`, `backend`, `roles` checkboxes, the
  per-role `enabled` keys (M01-12 mechanism, workspace tab).

### Mock daemon (`lib/mock.ts`)

A session with 8 steps of invocations (all roles, one bypass, one
regret, one compaction with `#state`, injected `#ctx`), streamed
`agent.apprentice` events during a mock run, `session.apprentice`,
`stats.tokens.apprentice` with `by_role`, `trace.blob` for a raw
result; `?apprentice=off` variant.

## Acceptance

- [ ] Mock: every step shows its strip; the flip on a compressed shell
      result shows raw and compressed side by side and a `ref L40-52`
      link scrolls the raw view to line 40; the bypass chip shows the
      reason; the regret mark appears on the expanded result; the
      compaction divider and the `#state` item render; the header
      totals equal the sum of the strips.
- [ ] Usage panel: the card's numbers match `stats.tokens.apprentice`
      of the mock, the by-role table sorts, the daily bar follows the
      range; `≈` appears on every estimated number and never on
      `tokens_in/out`.
- [ ] Composer switch off → the next `agent.run` carries `apprentice:
      false` (mock asserts) and the session header notes mixed runs;
      status bar shows `off` for that run and `observer` afterwards.
- [ ] Live events: during a mock run the strips appear per step as
      `agent.apprentice` arrives, before the next mentor text.
- [ ] Vitest: `lib/wire.ts` goldens (shared JSON with the Rust
      `wire::parse` snapshots), strip derivation (`lib/apprentice.ts`:
      chip text, totals, mixed-run detection), the card's aggregation;
      `pnpm typecheck`, `pnpm lint`; `api.ts` mirrors `session.
      apprentice`, `agent.apprentice`, `agent.compacted` with goldens
      from the Rust snapshots.
- [ ] Real daemon (`TESTING.md` M03 section): a dogfood session on the
      reference machine — flip a compressed cargo result, open the
      `#state` after a compaction, read the session total, toggle the
      apprentice off for one turn and back.

## Verification

`pnpm test`, `pnpm typecheck`, `pnpm lint`; the mock walkthrough; the
real-daemon checklist.

## Notes

- No business logic in the frontend (M00-10): the strip renders what
  the record says; chip text is derived, never computed from blobs.
- The strip is where M05's feedback controls land (thumbs on a
  compression, "this was over-compressed"); leave room in the chip
  layout and keep the record id on every chip's DOM node.
