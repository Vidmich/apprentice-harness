# M03-07 — Role: history compactor

Status: todo
Depends on: M03-02, M03-08, M01-10
Size: M

## Goal

When a session's history grows past a threshold, replace its older
turns with a `#state` document (goal, decisions, files touched, open
problems, what was tried) produced by the apprentice — from the observer
state (M03-02b) as a query, or statelessly from the transcript window —
so the mentor's input tokens stop growing with the transcript while
nothing the API requires is broken (thinking blocks, `tool_use` /
`tool_result` pairing) and the cached prefix before the cut stays
valid. The same mechanism can be switched to Anthropic's server-side
compaction so M04 can compare the two.

## Context

SPEC §5.3 (input: the conversation; output: a compact state document
"used in place of old turns"; success: mentor behaviour equivalence on
replay, no re-asking for facts; "complements, and can be compared
against, the Anthropic server-side compaction feature"), §6 (append-only:
"compaction replaces the tail of the history with a summary block,
never edits earlier turns"). M01-08's `Conversation { messages, validate,
repair, context_tokens }` and the context guards (`mentor.context_soft_
limit`); M01-10's `session_messages` write-through and `truncate_
session_messages`; `Role::System` in `mentor::types` "used by the
compactor"; M03-01's `#state` grammar; M03-08's injection.

## Scope

In: the trigger, the cut-point rules, the state document generation
(both backends), the replacement in `Conversation` and in
`session_messages`, resume of a compacted session, the trace records
and events, the server-side comparison switch, config, tests.
Out: the observer's own self-compaction (M03-02b), the `#state`
document as a per-turn injection without cutting history (M03-08 —
this task decides *when history is cut*), measuring equivalence (M04).

## Design

### Trigger

In `before_call`: `context_tokens()` (the last call's input incl. cache
reads) ≥ `compactor.trigger_tokens` (default 40 % of `mentor.context_
soft_limit`) **and** at least `compactor.min_steps_between` (6) steps
since the last compaction, **or** a `context_large` warning fired.
Priority `Background` below `compactor.urgent_tokens` (70 % of the soft
limit) — it runs while the current step's tools execute and applies at
the *next* step boundary — else `Interactive` before this call.

### Cut point

The history is `[user₀ (task), (assistant, user(tool_results))*, …]`.
Keep: `user₀` verbatim (the task; the `#workspace` system block is not
in messages), the last `compactor.keep_steps` (4) steps whole, and every
message after the cut. Replace: everything between, which always starts
at an `assistant` turn and ends at a `user` turn holding tool results —
so `tool_use` blocks and their `tool_result`s are removed together,
thinking blocks go with their assistant turn, and the remaining sequence
still alternates roles (`Conversation::validate` is run on the result
and a failure aborts the compaction with a record). Earlier `#state`
system messages (M03-08) inside the cut range are removed with it; ones
after the cut stay.

### The `#state` document

- Observer backend: `query(state)` — the state has seen everything; the
  answer is committed as `@state` (M03-02b).
- Stateless backend: prompt = `roles/compactor.md` + the previous
  `#state` (if any) + the turns being cut, rendered compactly (text
  verbatim, `tool_use` as `call <name> <args ≤ 200 chars>`, tool results
  as their `#result` blocks or the first 40 lines) up to `compactor.
  input_tokens` (24k; older turns beyond it are summarised by their
  `#result`/`call` lines only, and the record says `input_truncated`).

Output: one `#state v=<n> step=<n>` block (M03-01 grammar: `goal`,
`decisions`, `files` with what changed, `open`, `tried`) ≤
`compactor.max_output_tokens` (1500). Validation: parse ok, every
`files:` path exists in the workspace or in the cut turns' tool calls
(hallucinated paths dropped, `refs_invalid` counted), non-empty `goal`.

### Replacement

`Conversation::replace_range(from_row, to_row, Message::system(#state
block))`. Replacing the middle changes the prefix *from the cut point*
onwards, which is what SPEC §6 allows ("replaces the tail … never edits
earlier turns"): everything before the cut is byte-identical and stays
cached; the summary and the kept recent steps are the new tail, re-sent
once. `session_messages` is rewritten through M01-10's
`truncate_session_messages(keep = cut_row)` + appends of the summary and
the kept rows, so a resumed session loads the compacted history; the
removed rows are not lost — the trace events hold every message and
`session.compacted {from_row, to_row, summary_event_id, tokens_before,
tokens_after}` records the operation (a new event kind).

The next `mentor.request` is recorded as usual; M04 can rebuild the
pre-compaction request from the events for the comparison.

### Server-side comparison switch

```toml
[apprentice.compactor]
enabled = true
strategy = "apprentice"   # apprentice | server | none
trigger_tokens = 240000   # default 40 % of mentor.context_soft_limit when unset
urgent_tokens = 420000
keep_steps = 4
min_steps_between = 6
input_tokens = 24576
max_output_tokens = 1500
```

`strategy = "server"`: the mentor adapter passes the API's context-
management compaction parameter (the current `compact` edit with a
trigger at `trigger_tokens`) and the runtime records the server's
compaction blocks as it does any response content; the apprentice
compactor does not run. `none`: neither (the baseline). All three write
the same `session.compacted` record shape (`strategy` field) so M04's
comparison is one query.

### Records and surfaces

`apprentice.invocation {role: "compactor", trigger: threshold|warning|
urgent, tokens_before: <tokens of the cut turns>, tokens_after:
<summary tokens>, cut: {from_row, to_row, steps}, refs_invalid}`;
`session.compacted` as above; event `agent.compacted {session_id, step,
tokens_before, tokens_after}` to clients (M03-10 shows a divider in the
transcript). CLI `harness session compact <id> [--dry-run]` forces one
(prints the `#state` with `--dry-run`).

## Acceptance

- [ ] Cut-point unit tests: a 12-step synthetic history with thinking
      blocks and multi-tool steps compacts to `[user₀, system(#state),
      last 4 steps]`; `validate()` passes; a history whose cut would
      split a tool pair is impossible by construction (property test
      over random step shapes).
- [ ] Mock backend returning a scripted `#state`: the trigger fires at
      the threshold, `Background` below `urgent_tokens` applies at the
      next boundary, `Interactive` above applies before the call; the
      `mentor.request` after compaction has the summary as a `system`
      message (M03-08) and `tokens_before/after` are recorded.
- [ ] `session_messages` after compaction equals the conversation;
      restarting the daemon and resuming the session sends the compacted
      history; the removed messages remain as trace events and
      `session.compacted` links them.
- [ ] A hallucinated path in `files:` is dropped and counted; an
      unparsable output aborts the compaction (history unchanged,
      record `InvalidOutput`), and the run continues.
- [ ] `strategy = "server"` sends the API parameter (wiremock body
      assertion) and records `session.compacted {strategy: server}` when
      the fixture response carries a compaction block; `none` sends
      neither.
- [ ] Live: a long dogfood session (≥ 25 steps, > `trigger_tokens`)
      compacts once; the next call's `cache_read_input_tokens` ≥ the
      tokens before the cut point (prefix preserved), and the mentor
      does not re-ask for anything in `#state` (read the transcript;
      session id in the notes).

## Verification

`cargo test -p apprentice-core runtime::apprentice::compactor::`
(cut rules, mock, resume, wiremock for the server strategy); the live
session with the cache numbers.

## Notes

- The cut point is the whole difficulty: get the pairing rules right
  with a property test before writing a single prompt line. The API
  rejects a dangling `tool_use` for the entire request.
- `trigger_tokens` at 40 % of the soft limit is a guess; M02-07's
  numbers do not help here (this is mentor-side). M04's comparison run
  (apprentice vs server vs none on the same sessions) sets it.
