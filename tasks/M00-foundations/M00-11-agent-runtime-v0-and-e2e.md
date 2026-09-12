# M00-11 — Agent runtime v0 and end-to-end round trip

Status: done
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
   `record_mentor_response(at, call_id, &resp, cost_micros)`, `record_mentor_error(at, call_id, &err, retry_no, final)`;
   `cost_micros = stats::price_call(&resp.model, &resp.usage, &config.pricing)` from M00-07)
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

M00-08 shipped `apprentice_core::app::AppState` (loader, store, writer,
secrets, `mentor()`, `shutdown()` token, `register(router)`); add the
`AgentRegistry` to it and register `agent.*` there. Each agent's
`CancellationToken` must be a child of `state.shutdown()` so
`daemon.shutdown` / signals cancel it; the daemon's `finish` should then
wait for the registry to drain (before the trace flush) and the idle timer
should treat running agents as activity (`Server::idle_for` in
`crates/daemon/src/lifecycle.rs`). The M00-08 acceptance item "graceful
shutdown with an in-flight mentor call records `agent.finished{cancelled}`
and flushes queued events" is tested here.

M00-09 shipped `harness run` against a mock router (`crates/cli/tests/run.rs`
shows the event script it expects). The CLI reads the per-call cost from the
optional `cost_usd` field of `agent.usage` (added in M00-09, `None` when the
model has no pricing entry): fill it when emitting the event. The M00-09
acceptance item "the trace shows `agent.finished{cancelled}` after CTRL-C" is
asserted by the cancellation E2E here.

## Acceptance

- [x] E2E test (`crates/daemon/tests/e2e_hello.rs`): start daemon with temp
      `HARNESS_HOME` and `HARNESS_MENTOR_BASE_URL` → wiremock serving a
      recorded SSE fixture; run CLI `harness run "hello"`; assert: stdout has
      the streamed text; trace has `agent.started`, `user.message`,
      `mentor.request` (blob bytes == body wiremock received), `mentor.response`
      (usage == fixture), `assistant.message`, `agent.finished{ok}`;
      `mentor_calls` row has cost matching pricing; `stats.tokens` reflects it.
      *The base URL comes from the temp home's `config.toml` rather than the
      env var (same loader path). The `mentor_calls` row is asserted in
      `crates/core/tests/runtime.rs`; the CLI-visible parts, including
      `stats tokens`, here.*
- [x] Cancellation E2E: fixture streams slowly (wiremock delay); CTRL-C /
      `agent.cancel` → `agent.finished{cancelled}` and `mentor.error{cancelled}`.
      *CTRL-C against the real binaries; `agent.cancel` over RPC in the
      core test.*
- [x] Error E2E: 401 fixture → `agent.finished{error}` with kind
      `mentor_error`, CLI exit 2, message mentions authentication.
- [x] Two concurrent `harness run` invocations produce two agents with
      non-interleaved traces and correct per-session `seq`.
- [ ] Live smoke (`HARNESS_LIVE=1`, manual): real call succeeds; usage and
      cost visible via `harness stats tokens`. *Not run: no live key in this
      environment (the M00-05 live test is pending for the same reason).
      Everything up to the wire is exercised against wiremock with the
      recorded fixtures.*
- [x] `harness trace show <mentor.request id> --blob` prints the exact JSON
      body; a reviewer confirms cache_control placement and thinking/effort
      fields. *The E2E asserts `--json trace show --blob` equals the bytes
      wiremock received; the body is reviewed in the notes below.*

## Verification

Run the E2E suite; then the manual live smoke on the reference machine and
inspect the trace via CLI and the GUI Playground (M00-10).

## Notes

- This is the milestone exit. After it passes, M00 is done and M01 starts
  with tools and the real chat UI.
- Keep `runtime` free of Anthropic specifics beyond the `Mentor` trait;
  block types are shared but provider-specific quirks stay in the adapter.

## Completion notes (2026-09-12)

- **Runtime** (`crates/core/src/runtime/`): `mod.rs` has `AgentRegistry`
  (`Mutex<HashMap<AgentId, AgentHandle>>` plus a `TaskTracker`; no new
  dependency instead of `DashMap`), `AgentHandle` (`cancel` token, a
  `broadcast::Sender<Event>` of 1024 and a `watch` holding the final
  `agent.finished` event) and `run_agent`, which records the agent and
  `user.message`, spawns the turn and returns the handle **with a receiver
  subscribed before the task starts**, so the RPC forwarder cannot miss
  `agent.started`. `agent.rs` is the turn (`execute` → `turn`), `prompt.rs`
  the request layout, `rpc.rs` the three handlers. Handles stay valid after
  the agent ends and report the final event; the registry lists running
  agents only.
- **Request bytes**: instead of `complete_raw`, `Mentor` gained
  `request_body(&req)`. The adapter serialises deterministically: the
  runtime stores those bytes as the `mentor.request` blob and `complete`
  serialises again to the same bytes (the E2E asserts blob == bytes
  wiremock received). This keeps one entry point on the trait.
- **System prompt v0**: `crates/core/prompts/system_v0.md` (about 190
  tokens), `include_str!`, one cached system block; the single user block
  also carries `cache_control`, uniform with the multi-turn layout of M01.
- **Trace shape**: `user.message {text_len}` + text blob,
  `assistant.message {call_id, text_len, stop_reason, truncated}` + text
  blob (the M00-06 table plus `call_id` and `truncated`), then
  `mentor.request/response/error` through the M00-06 record helpers.
  `stop_reason == max_tokens` finishes `ok` with `truncated: true` on
  `agent.finished`, a new optional field of the API event (skipped when
  false, so the M00-02 snapshots are unchanged); the CLI prints a note and
  puts `truncated` in the `--json` result, the GUI shows it in the status
  label. `agent.usage` carries `cost_usd` from `stats::price_call` on the
  model the response reports. `mentor.error` is recorded once with
  `retry_no: 0`: the adapter retries internally and does not surface the
  intermediate attempts (a later task can thread them through).
- **RPC**: `agent.run` answers `{agent_id, subscription: agent_id}`; a
  detached task forwards events until the terminal one or the connection
  goes, and a dropped connection does not cancel the agent. `agent.cancel`
  on a finished but known agent is `Ok` (idempotent), unknown is
  `not_found`. `agent.subscribe` on a running agent forwards future events;
  on one that has ended (per the handle or the trace) it answers
  `running: false` and sends the terminal `agent.finished`, so the stream
  closes instead of hanging. A lagging subscriber gets a `log` warning
  naming the number of dropped events.
- **Shutdown**: agent tokens are children of `AppState::shutdown()`.
  `AppState::close()` now cancels, drains the registry (3 s, under the
  daemon's 10 s hard deadline with its 5 s connection drain) and then
  flushes the writer. A connection stays open until its forwarders are
  done, so the client of an in-flight run sees `agent.finished{cancelled}`
  before the daemon goes (E2E: `harness daemon stop` mid-run gives CLI exit
  130 "agent cancelled", the trace is flushed, a fresh daemon reads it).
  The idle timer treats running agents, and the moment the last one
  ended, as activity.
- **Tests**: `crates/core/tests/runtime.rs` (10: the round trip with every
  trace row and blob checked, cancellation, 401, missing key, max_tokens,
  unknown session, shutdown, and over an in-memory router two concurrent
  runs, cancel/subscribe semantics, connection drop);
  `crates/daemon/tests/e2e_hello.rs` (6, real binaries; `harness` is built
  on demand the way the CLI's daemon test builds `harnessd`); prompt unit
  tests; `just check` green.
- **Reviewed request body** (`trace show --blob` in the E2E home): model
  `claude-opus-5`, `max_tokens` 64000, `stream: true`, `system` = one text
  block with `cache_control: ephemeral`, `messages` = one user text block
  with `cache_control: ephemeral`, `thinking: {adaptive, display:
  summarized}`, `output_config: {effort: high}`, no `tools` key while the
  list is empty. Breakpoints and thinking/effort fields are where SPEC
  section 6 wants them.
- **GUI check** (manual, `pnpm tauri dev` with an isolated `HARNESS_HOME`
  whose `mentor.base_url` pointed at a small local server replaying
  `text.txt` one event per 0.8 s): the Playground streamed `Hello,` then
  `Hello, world!`, ended with `status: ok` and the usage line
  `in 25 · out 12 · cache read 0 · $0.0004 · 7.2s`; Cancel mid-stream
  gave `status: cancelled`, kept the partial text and showed
  `error: operation cancelled [cancelled]`; `harness trace list` on that
  home showed the seven-event round trip and the cancelled run's
  `mentor.error`, `harness stats tokens` the priced calls. That closes
  the two M00-10 items deferred to this task.
- Open: the live smoke (`HARNESS_LIVE=1`) against the real API is a manual
  step for the reference machine; the code path is the same as the
  wiremock run apart from endpoint and key.
