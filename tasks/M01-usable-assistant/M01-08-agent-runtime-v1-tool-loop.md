# M01-08 — Agent runtime v1: tool loop, multi-turn sessions, streaming

Status: done
Depends on: M00-11, M01-01, M01-07
Size: L

## Goal

The runtime becomes a real coding agent: multi-turn conversations per
session, the full mentor→tool→mentor loop with parallel tool execution,
permission gating, streaming of text/thinking/tool activity, cancellation
at any point, guards for iteration count and context size, and complete
trace capture of every step — the baseline (remote-only) behaviour the
apprentice will later be measured against.

## Context

SPEC §4 (the loop and the hook points the apprentice will use), §6 (prefix
stability: tools sorted, frozen system prompt, breakpoints), §9 (each step
= one mentor call + its tool executions). M00-11 provided the single-turn
skeleton; this task completes the loop and keeps the hook points explicit
(`before_call`, `on_tool_result`, `before_tool_exec`) as no-op traits that
M03 implements.

## Scope

In: conversation state, loop, tool dispatch, hooks, events, cancellation,
guards, error handling and recovery, `agent.run` continuing an existing
session, `agent.subscribe` reattachment, message construction rules for
the API (thinking echo, tool_result batching).
Out: session persistence format (M01-10 — but design together), system
prompt content (M01-09), apprentice roles (M03), sub-agents (M07).

## Design

### Conversation state

`Conversation { session_id, messages: Vec<Message>, tools: Vec<ToolDef>,
system: Vec<SystemBlock>, model, effort }` held in memory per open session
and rebuilt from the trace/session store on resume (M01-10). Each
`agent.run` on a session appends a user turn and runs the loop until
`end_turn`; the session's messages are the accumulated history.

### Loop

```
record agent.started, workspace.snapshot(start), user.message
loop (max_iterations from config, default 200):
    step = start_step
    hooks.before_call(&mut ctx)                       // M03: compactor/selector; no-op now
    req = build_request(conv)                         // cache breakpoints: last system block; last block of last user msg
    record mentor.request (exact bytes) ; call mentor with streaming → events (text_delta, thinking_delta, tool_call as blocks complete)
    record mentor.response, mentor_calls, assistant.message (text parts)
    push assistant message (ALL blocks incl. thinking with signatures, verbatim)
    match stop_reason:
      end_turn | stop_sequence → finish step, break
      max_tokens → finish step; push a user message "[continue — output was cut off]" once, then treat a second max_tokens as end (avoid loops)
      refusal → record outcome{error: refusal, category}; break with agent.finished{error: refusal}
      pause_turn → re-send as is (server tool loop) — not expected in M01, handle generically
      tool_use →
        results = execute_tools(blocks)               // M01-01 policy: read-only parallel, write/execute sequential; permission per call
        hooks.on_tool_result(&mut results)            // M03: compressor; no-op now
        push user message with ALL tool_result blocks in the same order; last block cache: true
        finish step; continue
record workspace.snapshot(end), outcome{files_changed}, agent.finished{status}
```

Tool errors (invalid input, denied, timeout) become `tool_result{is_error:
true}` with a short text — the loop never aborts on a tool failure; only
mentor errors, cancellation or guards end it.

### Streaming events (extends M00-02 `Event`)

`agent.tool_call {call_id, name, input}` when the block completes,
`agent.tool_progress {call_id, stream, text}` (shell), `agent.tool_result
{call_id, ok, summary, blob_id, mentor_bytes}`, `agent.step {seq, phase:
mentor|tools}`, `agent.usage` per call with running session totals, and
the existing text/thinking/finished events.

### Cancellation

`CancellationToken` checked: mid-stream (adapter aborts), between tools
(running tools get cancelled, results recorded as cancelled), before the
next call. Trace ends with `agent.finished{cancelled}`; the conversation
keeps the partial assistant message only if it had no pending `tool_use`
without results (otherwise drop the partial turn to keep the history valid
for the API).

### Guards

- `max_iterations` (config) → finish with `error: max_iterations`.
- Context size: before each call, if the last `usage.input_tokens +
  cache_read` exceeded `mentor.context_soft_limit` (default 600k), emit
  `agent.warning{context_large}`; at `context_hard_limit` (900k) stop with
  `error: context_limit` (the compactor, M03, later prevents this).
  `count_tokens` is used only when no previous usage exists (first call of a
  resumed session with a large history).
- Mentor `RateLimited`/`Overloaded` beyond adapter retries → wait per
  `retry_after` up to 5 min with `agent.waiting{reason, until}` events, then
  fail.
- `StreamInterrupted{partial}`: discard partial, retry the call once, then
  fail.

### Request construction rules

- `tools` sorted by name; identical bytes every call within a session
  (verify by hashing; log a warning if the hash changes mid-session — it
  breaks the cache).
- `system` frozen for the session (M01-09 builds it once at session start;
  changes apply from the next session).
- Thinking blocks are echoed exactly; if the model id changes mid-session
  (user switched), thinking blocks from the old model are kept as is (the
  API ignores them) — do not strip.
- Every request is serialised once; the bytes go to both the trace and the
  adapter (M00-11 rule).

## Acceptance

- [x] E2E with mock fixtures: a 3-step trajectory (text → tool_use ×2
      parallel → tool_use write → end_turn) produces the expected sequence
      of events and trace records; steps have `seq` 1..3; tool results are
      batched into one user message in order. —
      `core/tests/loop.rs::a_three_step_trajectory_streams_events_and_
      records_every_step` (events, request bodies, breakpoints, trace
      kinds, steps, blobs byte for byte, snapshots and the outcome) and
      `daemon/tests/e2e_loop.rs` (the real CLI and daemon, `harness
      trace list`).
- [x] Denied tool → `is_error` result; loop continues; mentor sees it. —
      `a_denied_tool_is_an_error_result_and_the_loop_goes_on`.
- [x] Cancel during tool execution and during streaming both leave a valid
      history (next `agent.run` on the session succeeds against the mock). —
      `cancelling_during_tools_and_during_streaming_leaves_a_valid_history`
      (each next run's request body passes `Conversation::validate`).
- [x] `max_tokens` continuation logic; `max_iterations` guard; context hard
      limit error. — `max_tokens_continues_once_then_ends_truncated`,
      `the_iteration_guard_stops_a_loop_that_never_ends`,
      `the_context_guards_warn_then_stop`.
- [x] Tools hash stable across a session (test asserts equal request
      `tool_names`/hash across steps). — the trajectory test (equal
      `tools` arrays in every body, equal `tool_names` in every
      `mentor.request`) and `conversation::tests::tool_set_changes_are_
      noticed_by_hash`.
- [ ] Live dogfood: implement a small feature in a scratch repo end to end
      via the GUI; every step visible in `harness trace list`. — the GUI
      chat view is M01-11; the CLI path is covered by `e2e_loop.rs` and a
      live `harness run` on a scratch repo (below). Left for M01-11's
      dogfood.

## Verification

`crates/daemon/tests/e2e_loop.rs` with fixtures; manual live session.

## Notes

- Keep the hook trait minimal now: `trait StepHooks { async fn
  before_call(&self, ctx: &mut CallContext); async fn on_tool_result(&self,
  ctx: &mut ToolResultContext); }` with a `NoopHooks` default. M03 swaps it
  for the apprentice orchestrator.

## Completion notes (2026-09-12)

`core::runtime` (`cargo test -p apprentice-core runtime`, `--test loop`
for the scripted trajectories, `cargo test -p harnessd --test e2e_loop`
for the CLI against the daemon):

- `conversation.rs` — `Conversation`: the messages of a session, the
  frozen system blocks, the sorted tool set and its SHA-256
  (`hash_tools`), the run's model/effort/max_tokens/thinking, the last
  call's usage and the running session totals (seeded from the store's
  `stats` for a session whose earlier calls happened in another daemon
  run). `request()` places the breakpoints (last system block, last
  tool, last block of the last user message) and stores none, so a
  request is the same bytes the API saw plus the tail. `push_user_text`
  joins a trailing user turn (a run cancelled mid-stream leaves one);
  `repair()` drops a trailing assistant turn with unanswered `tool_use`;
  `validate()` checks alternation and tool pairing (tests run it on
  every request body; M01-10 runs it on load). `set_tools` returns the
  old hash when the set changed mid-session → `agent.warning
  {kind: tools_changed}`. `load(session, messages)` is the entry point
  for M01-10's resume.
- `hooks.rs` — `StepHooks { before_call(CallContext), before_tool_exec
  (ToolExecContext), on_tool_result(ToolResultContext) }` with no-op
  defaults and `NoopHooks`; `run_agent_with(.., hooks)` takes another
  implementation (M03).
- `agent.rs` — the loop of the design: per iteration the context
  guard, `before_call`, a step row, `agent.step {seq, phase: mentor}`,
  `mentor.request` (bytes recorded once, sent as is), the streaming
  call (`agent.tool_call` when a `tool_use` block completes, its input
  parsed from the deltas), `mentor.response` + `assistant.message`
  (payload gained `tool_calls`) + `agent.usage` (with `session_usage`,
  `session_cost_usd`), the assistant turn pushed verbatim, then by stop
  reason: `tool_use` → `agent.step {phase: tools}`, `before_tool_exec`,
  the executor under a `PermissionGate` (engine per run, `AgentPrompter`
  with the handle as sink, live `permission.decision`), `agent.tool_
  progress` forwarded from the shell, `agent.tool_result` as each call
  ends (executor `Observer`), `on_tool_result`, one user message with
  every result in order; `max_tokens` → the tools if any arrived, else
  `[continue — output was cut off]` once (recorded as a `user.message`
  with `synthetic: "continue"`), a second one ends `ok, truncated`;
  `refusal` → `outcome {kind: error, details.kind: refusal, category}`
  and `agent.finished {error: refusal}`; `pause_turn` → the same
  conversation again; `model_context_window_exceeded` → `context_limit`;
  `end_turn` / `stop_sequence` / unknown → done. Around the loop:
  `workspace.snapshot` start/end and the `files_changed` outcome
  (sessions with a workspace; the CLI's default workspace is the
  current directory, so every CLI run has them).
- Guards: `runtime.max_iterations` (200) → `agent_stopped
  {kind: max_iterations}` after the last step completes (the history
  stays whole); `mentor.context_soft_limit` (600k) → `agent.warning
  {kind: context_large}` once per run; `mentor.context_hard_limit`
  (900k) → `context_limit` before the call; both measured as the last
  call's `input + cache_read + cache_creation`, or `count_tokens` when a
  resumed history has no usage yet. Runtime retries on top of the
  adapter's: a 429/529 waits `retry-after` (else 30 s) within
  `runtime.max_wait_s` (300) with `agent.waiting {reason, until,
  wait_ms}` and a non-final `mentor.error {retry_no}`; a stream broken
  after content is called once more (`agent.warning {kind:
  stream_interrupted}`; the partial deltas already went out).
- Cancellation: mid-stream → no assistant turn is pushed; during tools →
  the results (`cancelled` kind) are pushed so the history stays valid;
  before the next call → done. The step ends `cancelled`, the agent
  `cancelled` with the `cancelled` error.
- One agent per session at a time: a second `agent.run` on a busy
  session is `conflict` with the running `agent_id` in `details`. The
  registry keeps the `Conversation` of every session in memory
  (`AgentRegistry::conversation`), locked for the length of a run.
- API: events `agent.step`, `agent.tool_progress`, `agent.warning`,
  `agent.waiting` are new; `agent.tool_result` gained `name` and
  `mentor_bytes`, `agent.usage` the session totals; error code `-32031
  agent_stopped` with kinds `refusal`, `max_iterations`,
  `context_limit`. Config: `[runtime] max_iterations, max_wait_s`
  (workspace-overridable), `mentor.context_soft_limit`,
  `mentor.context_hard_limit`. CLI: `run --show-output` prints tool
  output as it streams; warnings and waits go to stderr. `api.ts` /
  `run.ts` mirror the events (`step` in the run state).

Deviations / decisions:

- `max_tokens` with `tool_use` blocks runs the tools rather than send
  the continuation message: a user text after an unanswered `tool_use`
  is invalid, and an input cut short becomes an error result the mentor
  reads.
- A denial, a timeout or an invalid input never ends the loop; only
  mentor errors, cancellation and the guards do (as designed). The
  hard context limit stops without also sending the soft warning.
- The `files_changed` outcome is recorded for every run with a
  workspace, empty lists included, so "nothing changed" is a label too.
- `count_tokens` is used only when a history exists without usage (a
  resumed session, M01-10); a fresh session's first call is never
  counted.
- The GUI dogfood box stays open until M01-11 (chat view) lands; the
  CLI equivalent is `e2e_loop.rs` plus a live scratch-repo run.
