# M01-11 — GUI chat view

Status: done
Depends on: M00-10, M01-08, M01-10
Size: L

## Goal

The main chat experience: a session transcript with streaming assistant
text rendered as markdown, code blocks with copy, collapsible thinking
summaries, tool-call cards (input, live progress for shell, result summary,
expand-to-raw from the trace blob), diff rendering for edits, the composer
with send/cancel, and keyboard shortcuts. This is what makes daily use
possible.

## Context

SPEC §13 first bullet. The GUI is a thin client (M00-10): everything shown
comes from RPC results and events; expanding a tool result fetches the
blob via `trace.get`.

## Scope

In: transcript rendering, streaming, tool cards, diff view, thinking
display, composer, cancel, error display, reattach on reload, virtualised
long transcripts, copy actions.
Out: sidebar/workspaces/permissions/settings (M01-12), token panel
(M01-13), feedback controls (M05).

## Design

### Data flow

- On session open: `session.get` (paged; newest page first, load older on
  scroll) → normalised into a `Transcript` store: turns → items
  `{kind: user|assistant_text|thinking|tool_call|tool_result|system_note|error}`.
- On `agent.run`: `rpc_stream` → events append/patch items:
  `text_delta` appends to the current assistant text item;
  `thinking_delta` to the current thinking item; `tool_call` creates a
  card; `tool_progress` appends to the card console; `tool_result`
  finalises the card; `usage` updates the turn footer; `finished` closes
  the turn.
- On window reload while an agent runs: `agent.subscribe` reattaches (no
  replay of past deltas — the stored messages fill the gap on next
  `session.get`).

### Rendering

- Markdown via `react-markdown` + `remark-gfm`; code via Shiki (bundled
  grammars for common languages, lazy-loaded); copy button per block;
  links open externally (`tauri-plugin-shell` `open`).
- Thinking: collapsed line "Thinking… (n chars)" that expands; hidden
  entirely if config `mentor.thinking_display = omitted`.
- Tool card: header `<icon> <name> <summary>` with status (running/ok/
  error/denied), body tabs: *Input* (pretty JSON), *Result* (text as sent
  to mentor, monospace, with the truncation marker highlighted), *Raw*
  (lazy: `trace.get {event_id, include_blob}`; virtualised for large
  outputs), and for `edit_file`/`write_file` a *Diff* tab rendered from the
  unified diff (side-by-side toggle).
- Shell card streams progress into a console area with autoscroll and a
  "follow" toggle.
- Turn footer: model, duration, tokens (in/out/cache read) and cost for
  that turn, from `agent.usage`.
- Errors: inline system note with `data.kind` and a "Retry" action
  (re-runs the last user prompt) where safe (no partial tool effects).
- Virtualisation (`@tanstack/react-virtual`) for transcripts > 200 items.

### Composer

Multiline textarea (Enter = send, Shift+Enter = newline; configurable),
paste of large text becomes an attached block shown as a chip and sent as
part of the prompt, Cancel button while running (`agent.cancel`), `Ctrl+L`
focuses the composer, `Esc` cancels. Disabled with a hint when no
workspace is selected.

### State

Zustand stores: `sessions` (list + active), `transcript` (per session,
LRU 5), `run` (active agent per session). All RPC through `lib/rpc.ts`.

## Acceptance

- [x] Streaming text renders without flicker at ≥ 60 events/s; markdown
      updates incrementally (no re-parse of the whole transcript per delta:
      parse only the streaming item). — events are folded in 16 ms batches
      (`chat.ts`), one store update per batch; every item is memoised on
      its own text, so a delta re-renders the streaming item alone; a
      code block keeps its last tokens and renders the new tail plain
      until the debounced re-tokenise lands. Checked against the mock
      daemon at ~60 deltas/s (3 chars per 16 ms).
- [x] Tool cards for each M01 tool render correctly, including a 5 MB shell
      output in *Raw* without freezing the UI. — `ToolCard` (Input /
      Result / Raw / Diff / Console); *Raw* is a virtualised line list
      (`RawView`): the mock's 5 MB output (80,693 lines) renders 39 rows
      and scrolls. The read/edit/shell cards were checked with the mock;
      the other tools share the same card (their results are text).
- [x] Diff tab shows a correct diff for `edit_file` results. — `diff.ts`
      (`extractDiff`, `parseDiff`, `sideBySide`; `diff.test.ts`) and
      `DiffView` (unified or side by side, line numbers). Also for
      `write_file` results that carry a diff.
- [x] Reload during a run reattaches; transcript is complete afterwards. —
      `reloadSession` finds the running agent in `session.get`'s `agents`
      and `reattach`es through `agent.subscribe`; the rows stored at the
      next step boundary replace the partial overlay
      (`transcript.test.ts` "hands over to the stored rows"; checked live
      with the mock, whose state survives a reload).
- [x] Cancel works mid-stream and mid-tool. — `Esc` / Cancel →
      `agent.cancel`; the reducer ends the running calls with the agent
      (`transcript.test.ts` "records a denied call, warnings, a cancelled
      run and its unanswered calls"). The live half (the daemon's
      cancellation under a tool) is `core/tests/loop.rs` and the
      checklist.
- [x] Keyboard shortcuts as specified; accessibility: focus order and
      ARIA roles for cards; dark and light themes. — Enter / Shift+Enter
      (or Ctrl+Enter, persisted), `Ctrl+L`, `Esc`; cards are `group`s
      named "<tool> tool call, <status>" with `tablist`/`tab`/`tabpanel`
      and `aria-expanded` headers; the transcript is a `log`; footers
      `contentinfo`, errors `alert`. Both themes through the existing
      `light-dark()` tokens; Shiki emits both themes' colours as CSS
      variables and `index.css` picks by scheme (checked in both).

## Verification

Vitest unit tests for the transcript reducer (events → items); Playwright
component smoke via `tauri-driver` is optional; manual dogfooding
checklist in `apps/gui/TESTING.md`.

## Notes

- Do not implement any logic that decides what the mentor sees — the
  daemon does that; the GUI only displays.

## Completion notes (2026-09-13)

`pnpm test` (26 vitest tests: `transcript.test.ts`, `diff.test.ts`, the
api/rpc drift tests), `pnpm typecheck`, `pnpm lint`; the Rust side in
`cargo test -p apprentice-api --test snapshots` and `-p apprentice-core
--test sessions`.

- Data flow as designed, with one difference in shape: the transcript is
  the stored rows (`session.get`, newest page first via the new
  `before_seq`, older pages on scroll) plus a *live overlay* — the prompt
  until its row is stored, the streaming steps until their assistant
  message is — folded from the events and shrunk as the rows arrive at
  the step boundaries (`agent.step {phase: mentor}` and `agent.finished`
  trigger a refresh with `after_seq`). Facts the rows do not carry (a
  call's summary, streamed output, trace event; a run's usage, warnings,
  error) live beside them keyed by call id and agent id, so a refresh
  never loses them and a card keeps its state across the hand-over (its
  key is the call id either way). `lib/transcript.ts` is pure and
  tested; `lib/chat.ts` holds the effects.
- API additions (Rust + `api.ts`, snapshots updated): `session.get`
  takes `before_seq` (the newest page first; `has_more` then means older
  rows precede it) and returns `agents` (`AgentSummary`: status,
  started/ended, model, calls, usage, cost per agent) so a loaded
  transcript has its turn footers and the reload knows which agent to
  subscribe to; `agent.tool_result` carries `event_id` (the
  `tool.result` trace event) so the Raw tab of a live card needs no
  search. For loaded cards the event is found through `trace.list
  {agent_id, kinds: [tool.result]}` + `trace.get` (cached per call).
  `harness session show --tail` uses the backwards paging.
- Rendering: `react-markdown` + `remark-gfm`; Shiki's fine-grained
  bundle (`shiki/core`, the JavaScript regex engine, `github-light` /
  `github-dark`) loads on the first code block, each grammar on first
  use, ~30 languages with aliases; links open through
  `tauri-plugin-opener` (the successor of `tauri-plugin-shell`'s
  `open`, capability `opener:default`); copy buttons on code blocks,
  answers, inputs, results and raw output.
- Thinking: a collapsed "Thinking… (n chars)" line; hidden entirely when
  `mentor.thinking_display` is `omitted` (read at bootstrap with the
  model). Redacted thinking shows as such.
- Composer: Enter sends / Shift+Enter breaks (or Ctrl+Enter; the choice
  persists), pastes ≥ 12 lines or ≥ 1500 chars become chips sent inside
  `<pasted_text>` after the typed text, Cancel while running, `Ctrl+L`
  focuses, `Esc` cancels, disabled with a hint until a workspace is
  chosen (the session is created on the first send, so the daemon names
  it after the prompt).
- Stores: `store.ts` (app state, the chats — persisted to `localStorage`
  so a reload finds its sessions — composer settings) and
  `stores/transcripts.ts` (per session, five most recently shown kept,
  a busy one never evicted). The run state lives in the transcript's
  overlay rather than a third store.
- The Playground (M00-10) is replaced by the chat; the sidebar lists the
  open chats. Sessions list, workspace picker and settings stay M01-12.
- `apps/gui/TESTING.md` is the manual checklist; `src/lib/mock.ts` is a
  scripted daemon the page uses outside Tauri (`pnpm dev` in a browser)
  — sessions, a canned streamed run with the three kinds of tool card,
  cancel, reattach after a reload, the trace behind Raw — so the view can
  be worked on and eyeballed without tokens.

Deviations / decisions:

- Turn footers of loaded sessions come from `session.get`'s `agents`
  (an API addition), not only from `agent.usage`; otherwise a reload
  would lose every footer.
- Paging is backwards from the end (`before_seq`, a new parameter)
  rather than forwards from `message_count`, which the client does not
  know before the first call.
- Retry re-sends the prompt as a new `agent.run` (the daemon joins it to
  the pending user turn: the mentor then sees the text twice). It is
  offered only for a turn that ran no tool. A `retry` option that reuses
  the pending turn would be a daemon change; noted for later. Once
  retried, the failed turn's footer goes: the merged user row belongs to
  the new agent, so the old one has no item left to hang it on.
- After a reattach the deltas streamed before the reload are not
  replayed (as designed); the current step shows what arrives from then
  on and the stored message replaces it at the step's end.
- The tool results row is stored after every tool of a step, so a live
  card's *Result* text appears at the next step boundary; until then the
  card shows the event's summary and, for the shell, the console.
- Virtualisation kicks in above 200 items; below, the list renders
  plainly (one code path per item either way). *Raw* is always
  virtualised.
- Events are batched on a 16 ms timer rather than `requestAnimationFrame`
  (a hidden window would stall the stream entirely).
- The GUI dogfood box of M01-08 (a small feature end to end via the GUI)
  remains for the first real run through `TESTING.md`; the view was
  exercised against the mock daemon here, not against the API.
