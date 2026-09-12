# M01-11 — GUI chat view

Status: todo
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

- [ ] Streaming text renders without flicker at ≥ 60 events/s; markdown
      updates incrementally (no re-parse of the whole transcript per delta:
      parse only the streaming item).
- [ ] Tool cards for each M01 tool render correctly, including a 5 MB shell
      output in *Raw* without freezing the UI.
- [ ] Diff tab shows a correct diff for `edit_file` results.
- [ ] Reload during a run reattaches; transcript is complete afterwards.
- [ ] Cancel works mid-stream and mid-tool.
- [ ] Keyboard shortcuts as specified; accessibility: focus order and
      ARIA roles for cards; dark and light themes.

## Verification

Vitest unit tests for the transcript reducer (events → items); Playwright
component smoke via `tauri-driver` is optional; manual dogfooding
checklist in `apps/gui/TESTING.md`.

## Notes

- Do not implement any logic that decides what the mentor sees — the
  daemon does that; the GUI only displays.
