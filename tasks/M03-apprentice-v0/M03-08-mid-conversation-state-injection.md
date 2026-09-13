# M03-08 — Mid-conversation state injection

Status: todo
Depends on: M03-01, M01-08
Size: S

## Goal

One mechanism — the `Injector` — through which every before-call block
(`#state` from the compactor or the observer, `#ctx` from the selector,
an `#ask-apprentice` answer without a tool turn) enters the mentor's
conversation: as a mid-conversation `role: "system"` message appended
at the tail (Opus 5 accepts them inside `messages`), so no earlier byte
changes and the cached prefix survives; with a fallback to a text block
prepended to the next user turn for models that reject system messages.
The cache effect is verified live, not assumed.

## Context

SPEC §6 ("Stable prefix layout … then compacted state, then the volatile
tail"; "Append-only prompts … Roles therefore only ever *append* or
*replace a suffix*"; "system text is frozen for the life of a session").
`mentor::types::Role::System` exists ("the mid-conversation system
message the current models accept inside `messages` (used by the
compactor)") and `Conversation::validate` already tolerates it in the
alternation check; `Conversation::request` puts the cache breakpoint on
the last block of the last user message. M03-02's orchestrator collects
the roles' blocks and hands them to the injector.

## Scope

In: the injector and its two placements, capability detection per
model, the cache-breakpoint rule with system messages, `session_
messages` persistence of injected messages, the trace record, the live
cache verification, config.
Out: what the blocks say (the roles), when history is cut (M03-07), the
GUI rendering of injected messages (M03-10 shows them as apprentice
items, not as user text).

## Design

### Placement

`Injector::inject(conv, blocks) -> Injected` appends, before the call,
one `Message { role: System, content: [Text(blocks joined)] }` at the
tail — after the last user message (tool results or the new user turn).
The cache breakpoint moves to this system message's block (it is now
the last message), so the previous request's prefix, including the user
message before it, is fully reused. Injected messages are **kept** in
the history (append-only: removing one later would invalidate the cache
from that point); M03-07's compaction removes them with the turns it
cuts, and a `#state` injected by the compactor is by definition the
first message after the cut.

Because kept blocks accumulate, injection is rationed by the roles'
triggers (M03-06/07) and by `injector.max_state_per_run` (3): a `#state`
refresh without a compaction is only injected when the observer reports
a changed state (`#state v` bumped) and the last one is ≥ `injector.
min_gap_steps` (8) old.

### Fallback

`mentor.system_messages = "auto" | "on" | "off"`. `auto`: at the
session's first injection the adapter tries the system message; an API
400 naming the role (or a model in `mentor::models::NO_SYSTEM_MESSAGES`)
flips the session to the fallback and the request is retried once
without a second mentor call being recorded as a step (`mentor.error`
with `kind: system_message_unsupported`, then the retry). Fallback
placement: the blocks as the **first** text block of the next user
message (before tool results — content order within a message does not
affect pairing), with the cache breakpoint still on the last block.
Both placements produce the same `Injected { placement: system|user,
rows: [..] }` record.

### Persistence and records

Injected messages are written to `session_messages` like every other
message (M01-10), tagged `origin: apprentice` in the row's JSON so the
GUI/transcript can distinguish them and `session resume` re-sends them
verbatim. Event `apprentice.injected {step, placement, blocks: [kind…],
bytes, rows}`; the `mentor.request` payload gains `injected: [event
ids]`.

### Live cache verification

A `just live-cache` recipe runs a scripted 4-step session on the
reference account: step 2 injects a `#state` via system message, step 3
via the user-block fallback (forced with `system_messages = off`); for
each, the assertion is `cache_read_input_tokens(step n) ≥ input_tokens
(step n−1) − 64` (the tail beyond the previous breakpoint is the only
uncached part). Results go into `docs/cache-behaviour.md` with the
model id and date — the README's open decision ("whether compactor
output goes via `role: system` messages or a user block, after testing
cache behaviour live") is closed by that file.

## Acceptance

- [ ] Unit: `inject` appends one system message at the tail; the
      request's only message-level breakpoint is on it; `validate()`
      passes with system messages between user and assistant turns;
      the rationing rules hold (state refresh gated by version and gap).
- [ ] Fallback: a wiremock 400 mentioning `role` flips the session to
      user placement, the retry succeeds, one `mentor.error` with the
      kind is recorded, and the user message has the blocks first;
      `system_messages = off` uses the fallback from the start.
- [ ] `session_messages` holds injected rows with `origin: apprentice`;
      resume re-sends them at the same positions (request golden).
- [ ] `apprentice.injected` and `mentor.request.injected` link to each
      other; `harness trace show` prints injected blocks in place.
- [ ] `just live-cache` passes for both placements on `claude-opus-5`
      and the numbers are committed in `docs/cache-behaviour.md`.

## Verification

`cargo test -p apprentice-core runtime::apprentice::inject::` (unit +
wiremock); `just live-cache` on the reference account.

## Notes

- Do not put anything volatile (time, step number) into an injected
  block's first bytes; a `#state` header with `step=<n>` is fine because
  the block is new anyway, but a re-sent `#state` must be byte-identical
  to the previous one or it is a new block.
- If the live check shows system messages break the cache on some
  model, `auto` must prefer the user block for that model — record the
  model id in `NO_SYSTEM_MESSAGES`, not a config default.
