# M00-11 — Agent runtime v0 and end-to-end round trip

Status: todo
Depends on: M00-05, M00-06, M00-07, M00-08
Size: M

## Goal

`apprentice_core::runtime` runs a single-turn agent (no tools yet): it builds
the mentor request with a cache-friendly layout, records the exact request
bytes and the response in the trace store, streams events to the RPC
subscriber, records usage/cost, and supports cancellation. With this, the M00
exit criterion holds: `harness daemon start` + `harness run "hello"`
round-trips through the mentor and the full exchange is in the trace store.

## Context

SPEC §4 (agent loop, remote-only baseline), §6 (stable prefix layout), §9
(replay guarantee). M01 extends this runtime with tools, multi-turn sessions
and permissions; keep the structure ready for that (steps, tool hooks) but
do not implement them here.

## Scope

In: `AgentRegistry`, `Agent` task, request building, system prompt v0,
trace recording, event fan-out, cancellation, `agent.run`/`agent.cancel`
RPC handlers, end-to-end test with mock mentor, one live smoke test.
Out: tools, multi-turn history, permissions, apprentice hooks (M01/M03).

## Design

### Structures

```rust
pub struct AgentRegistry { agents: DashMap<AgentId, AgentHandle> }
pub struct AgentHandle { cancel: CancellationToken, events: broadcast::Sender<Event>, status: watch::Receiver<AgentStatus> }
pub struct RunOptions { model: Option<String>, effort: Option<Effort>, apprentice: Option<bool> }
pub async fn run_agent(state: Arc<AppState>, session: SessionId, prompt: String, opts: RunOptions) -> Result<AgentId>
```

`run_agent` records `agent.started` and `user.message`, spawns a task, and
returns immediately; the RPC handler returns `{agent_id, subscription:
agent_id}` and attaches the connection to the broadcast channel.

### Request building (`runtime/prompt.rs`)

Order and content, chosen for cache stability (SPEC §6):

1. `tools`: empty in M00 (M01 adds; must be sorted by name).
2. `system` blocks:
   - block 0: **frozen** system prompt v0 (text file
     `crates/core/prompts/system_v0.md`, embedded with `include_str!`), no
     interpolation, `cache: true`.
   - block 1 (M01): workspace context; not present in M00.
3. `messages`: `[{user: prompt}]`; the last block of the last user message
   gets `cache: true` from M01 on (multi-turn); in M00 a single short prompt
   is below the cacheable minimum anyway — still set it for uniformity.
4. `thinking: adaptive{display from config}`, `effort` from options/config,
   `max_tokens` from config, `model` from options/config.

The body is serialised once (`serde_json::to_vec`), stored as the
`mentor.request` blob, and the same bytes are given to the adapter
(`Mentor::complete` takes `&MentorRequest`; add `AnthropicMentor::complete_raw(bytes)`
or make the adapter serialise deterministically and assert equality in the
test — choose the former: the adapter accepts prebuilt bytes plus the typed
request for validation).

System prompt v0 content: identity ("You are the mentor model inside
apprentice-harness, a coding assistant harness"), that tools will be
available in later versions, concise-answer instructions. Keep it under 300
tokens; it is replaced in M01-09.

### Execution

```
step = trace.start_step(agent)
call_id = uuid7
trace.append(mentor.request{...}, blob=body)      ; trace.record_mentor_call(started)
  (M00-06 ships these as one call each: `record_mentor_request(at, call_id, &req, &body)`,
   `record_mentor_response(at, call_id, &resp, cost_micros)`, `record_mentor_error(at, call_id, &err, retry_no, final)`)
resp = mentor.complete(req, |ev| forward(ev), cancel)
  on TextDelta      → events.send(agent.text_delta)
  on ThinkingDelta  → events.send(agent.thinking_delta)
  on Usage          → (final) events.send(agent.usage)
match resp:
  Ok(r)  → trace.append(mentor.response{usage,...}, blob=content json); trace.complete_mentor_call(ok, usage, cost); trace.append(assistant.message, blob=text); finish step ok; agent.finished{ok}
  Err(Cancelled) → mentor.error{cancelled}; complete_mentor_call(cancelled); agent.finished{cancelled}
  Err(e) → mentor.error{kind,...}; complete_mentor_call(error); agent.finished{error: RpcError}
```

Cost via M00-07 using the model actually reported in the response.
`stop_reason == max_tokens` → finish `ok` but include `truncated: true` in
the `agent.finished` event and the `assistant.message` payload.

### RPC

`agent.run` → `run_agent`; `agent.cancel` → `handle.cancel.cancel()`;
connection drop does NOT cancel the agent (the CLI's CTRL-C cancels
explicitly; the GUI may reattach — `agent.subscribe {agent_id}` method added
here for reattachment, replaying nothing, just future events).

## Acceptance

- [ ] E2E test (`crates/daemon/tests/e2e_hello.rs`): start daemon with temp
      `HARNESS_HOME` and `HARNESS_MENTOR_BASE_URL` → wiremock serving a
      recorded SSE fixture; run CLI `harness run "hello"`; assert: stdout has
      the streamed text; trace has `agent.started`, `user.message`,
      `mentor.request` (blob bytes == body wiremock received), `mentor.response`
      (usage == fixture), `assistant.message`, `agent.finished{ok}`;
      `mentor_calls` row has cost matching pricing; `stats.tokens` reflects it.
- [ ] Cancellation E2E: fixture streams slowly (wiremock delay); CTRL-C /
      `agent.cancel` → `agent.finished{cancelled}` and `mentor.error{cancelled}`.
- [ ] Error E2E: 401 fixture → `agent.finished{error}` with kind
      `mentor_error`, CLI exit 2, message mentions authentication.
- [ ] Two concurrent `harness run` invocations produce two agents with
      non-interleaved traces and correct per-session `seq`.
- [ ] Live smoke (`HARNESS_LIVE=1`, manual): real call succeeds; usage and
      cost visible via `harness stats tokens`.
- [ ] `harness trace show <mentor.request id> --blob` prints the exact JSON
      body; a reviewer confirms cache_control placement and thinking/effort
      fields.

## Verification

Run the E2E suite; then the manual live smoke on the reference machine and
inspect the trace via CLI and the GUI Playground (M00-10).

## Notes

- This is the milestone exit. After it passes, M00 is done and M01 starts
  with tools and the real chat UI.
- Keep `runtime` free of Anthropic specifics beyond the `Mentor` trait;
  block types are shared but provider-specific quirks stay in the adapter.
