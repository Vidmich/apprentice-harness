# M01-08 — Agent runtime v1: tool loop, multi-turn sessions, streaming

Status: todo
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

- [ ] E2E with mock fixtures: a 3-step trajectory (text → tool_use ×2
      parallel → tool_use write → end_turn) produces the expected sequence
      of events and trace records; steps have `seq` 1..3; tool results are
      batched into one user message in order.
- [ ] Denied tool → `is_error` result; loop continues; mentor sees it.
- [ ] Cancel during tool execution and during streaming both leave a valid
      history (next `agent.run` on the session succeeds against the mock).
- [ ] `max_tokens` continuation logic; `max_iterations` guard; context hard
      limit error.
- [ ] Tools hash stable across a session (test asserts equal request
      `tool_names`/hash across steps).
- [ ] Live dogfood: implement a small feature in a scratch repo end to end
      via the GUI; every step visible in `harness trace list`.

## Verification

`crates/daemon/tests/e2e_loop.rs` with fixtures; manual live session.

## Notes

- Keep the hook trait minimal now: `trait StepHooks { async fn
  before_call(&self, ctx: &mut CallContext); async fn on_tool_result(&self,
  ctx: &mut ToolResultContext); }` with a `NoopHooks` default. M03 swaps it
  for the apprentice orchestrator.
