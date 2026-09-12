# M00-05 — Mentor adapter: Anthropic Messages API

Status: done
Depends on: M00-01, M00-03
Size: L

## Goal

`apprentice_core::mentor` provides a `Mentor` trait and an `AnthropicMentor`
implementation that sends Messages API requests over raw HTTPS, always
streams, supports tool use, adaptive thinking with effort, prompt-cache
breakpoints, retries and cancellation, and returns a fully assembled response
with exact `usage`. It also exposes `count_tokens`. The adapter is pure
(no trace writes); the runtime records around it.

## Context

SPEC §3.1 Mentor adapter, §6 (cache-friendly prefix layout), §12.2 (usage is
the raw signal for token metrics). There is no official Rust SDK, so the
adapter implements the wire protocol directly. Mentor model id, effort,
max_tokens and base URL come from config (M00-03).

## Scope

In: request/response types, SSE streaming parser, assembly of content blocks,
tool definitions and tool results, thinking blocks round-trip, cache_control,
usage capture, typed errors, retry/backoff, cancellation, `count_tokens`,
mock-server tests.
Out: Batch API, server-side compaction/context editing, server tools
(web search etc.), non-Anthropic providers, fallbacks parameter.

## Design

### Wire facts (verified 2026-09; re-verify against docs when in doubt)

- `POST {base_url}/v1/messages`, headers `content-type: application/json`,
  `x-api-key: <key>`, `anthropic-version: 2023-06-01`. Beta features add
  `anthropic-beta: <ids>` — none needed in M00.
- Body: `model`, `max_tokens`, `stream: true`, `system` (array of
  `{type:"text", text, cache_control?}`), `messages` (array of
  `{role: "user"|"assistant", content: string | [blocks]}`), `tools`
  (array of `{name, description, input_schema, cache_control?}`),
  `thinking: {type:"adaptive", display:"summarized"|"omitted"}`,
  `output_config: {effort: "low"|"medium"|"high"|"xhigh"|"max"}`,
  optional `metadata: {user_id}`.
- Content block types: `text {text}`, `tool_use {id, name, input}`,
  `tool_result {tool_use_id, content: string|[blocks], is_error?}`,
  `thinking {thinking, signature}`, `redacted_thinking {data}`,
  `image`/`document` (types defined, unused in M00).
- `thinking` blocks returned by the model must be echoed back **unchanged**
  (including `signature`) in the assistant message on later turns of the same
  model; never edit or drop them. No assistant prefill (400 on current models).
- SSE events: `message_start` (carries `message.usage` with `input_tokens`,
  `cache_read_input_tokens`, `cache_creation_input_tokens`),
  `content_block_start {index, content_block}`, `content_block_delta {index,
  delta}` with delta types `text_delta {text}`, `input_json_delta
  {partial_json}`, `thinking_delta {thinking}`, `signature_delta
  {signature}`, `content_block_stop {index}`, `message_delta {delta:
  {stop_reason, stop_sequence}, usage: {output_tokens, ...}}`, `message_stop`,
  `ping`, `error {error: {type, message}}`.
- `stop_reason`: `end_turn`, `tool_use`, `max_tokens`, `stop_sequence`,
  `refusal`, `pause_turn`. On `refusal`, `stop_details {type, category,
  explanation}` may be present — capture it.
- Cache control: `cache_control: {type:"ephemeral"}` (optionally
  `ttl: "1h"`) on at most 4 blocks. Render order tools → system → messages;
  a breakpoint on the last system block caches tools+system; a breakpoint on
  the last block of the latest user turn makes conversations cache
  incrementally.
- `POST /v1/messages/count_tokens` with `{model, system?, tools?, messages}`
  → `{input_tokens}`.
- Errors: HTTP 400 invalid_request, 401 authentication, 403 permission,
  404 not_found, 413 request_too_large, 429 rate_limit (`retry-after`
  header), 500 api_error, 529 overloaded. Body `{type:"error", error:{type,
  message}}`.

### Types (`mentor/types.rs`)

```rust
pub struct MentorRequest { pub model: String, pub max_tokens: u32, pub system: Vec<SystemBlock>, pub messages: Vec<Message>, pub tools: Vec<ToolDef>, pub thinking: Thinking, pub effort: Effort, pub metadata: Option<Metadata> }
pub struct SystemBlock { pub text: String, pub cache: bool }
pub struct Message { pub role: Role, pub content: Vec<ContentBlock> }   // always block form when serialised
pub enum ContentBlock { Text{text, cache}, ToolUse{id,name,input: Value}, ToolResult{tool_use_id, content: Vec<ToolResultContent>, is_error: bool, cache: bool}, Thinking{thinking, signature}, RedactedThinking{data} }
pub struct ToolDef { pub name, pub description, pub input_schema: Value, pub cache: bool }
pub enum Thinking { Adaptive{display: ThinkingDisplay} }
pub enum Effort { Low, Medium, High, XHigh, Max }
pub struct MentorResponse { pub id: String, pub model: String, pub content: Vec<ContentBlock>, pub stop_reason: StopReason, pub stop_details: Option<StopDetails>, pub usage: Usage, pub timing: Timing /* first_byte_ms, total_ms */, pub request_bytes: usize, pub raw_sse: Option<Vec<u8>> }
pub struct Usage { input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens }   // u64
pub enum StreamEvent { TextDelta(String), ThinkingDelta(String), ToolUseStart{index, id, name}, ToolInputDelta{index, partial_json}, BlockStop(index), Usage(Usage), Done }
```

Serialisation uses `serde` with `#[serde(tag="type")]`; `cache: bool` renders
as `cache_control: {type:"ephemeral"}` when true. Tools are serialised in the
order given (the runtime sorts by name for cache stability — adapter does not
reorder).

### Trait

```rust
#[async_trait]
pub trait Mentor: Send + Sync {
    async fn complete(&self, req: &MentorRequest, on_event: &mut (dyn FnMut(StreamEvent) + Send), cancel: CancellationToken) -> Result<MentorResponse, MentorError>;
    async fn count_tokens(&self, req: &MentorRequest) -> Result<u64, MentorError>;
    fn model_id(&self) -> &str;
}
```

`AnthropicMentor::new(config: &MentorConfig, secret: Secret, http: reqwest::Client)`.

### Streaming implementation

- `reqwest` with rustls, `bytes_stream()`; a small SSE parser (`event:` /
  `data:` lines, blank-line delimited, ignore comments) — write it in-house
  (~80 lines) to avoid an extra dependency; unit-test with split chunks.
- Assembler keeps `Vec<PartialBlock>` indexed by `index`; `input_json_delta`
  fragments are concatenated and parsed with `serde_json` at
  `content_block_stop` (invalid JSON → `MentorError::Protocol`).
- Usage: take input/cache fields from `message_start`, output from the last
  `message_delta` (it is cumulative). Missing fields → 0.
- `on_event` is called for every delta so the GUI streams text; it must be
  cheap (the runtime forwards to a channel).
- If `trace.capture_raw_sse` is set, keep the raw bytes in `raw_sse`.

### Retries and cancellation

- Retry on 429, 500, 529, connection errors and on stream interruption
  **before any content block started**; exponential backoff 1s·2^n with
  jitter, capped at 60s, max `mentor.max_retries`; honour `retry-after`.
- A stream interrupted after content began is NOT retried (the partial
  content would be lost/duplicated) → `MentorError::StreamInterrupted{partial}`;
  the runtime decides.
- `CancellationToken` cancels the in-flight request; returns
  `MentorError::Cancelled`. Bytes received so far are discarded by the
  adapter; the runtime records the cancellation.

### Errors

```rust
pub enum MentorError { Auth, RateLimited{retry_after: Option<Duration>}, Overloaded, InvalidRequest{message}, RequestTooLarge, Api{status: u16, kind: String, message: String}, Network(reqwest::Error), Protocol(String), StreamInterrupted{partial: Vec<ContentBlock>}, Cancelled, Timeout }
```

Maps to RPC codes -32020/-32021 in the daemon.

## Acceptance

- [x] Against a `wiremock` server replaying recorded SSE fixtures: text-only
      response, response with two tool_use blocks (parallel), response with
      thinking blocks, `max_tokens` stop, `refusal` with `stop_details`,
      error event mid-stream, 429 then success, 529 then success, 401.
- [x] Usage assembled exactly from fixtures, including cache fields.
- [x] `count_tokens` works against the mock.
- [x] Cancellation mid-stream returns `Cancelled` within 100 ms and drops the
      connection.
- [x] Serialised request JSON for a sample request matches a snapshot that a
      reviewer has checked against the API docs (system array, tools with
      cache_control, adaptive thinking, output_config.effort, no prefill).
- [ ] One `#[ignore]` live test (`HARNESS_LIVE=1`) performs a real call with
      a 20-token prompt and asserts `usage.input_tokens > 0`.

## Verification

`cargo test -p apprentice-core mentor::` plus the ignored live test run once
manually with a real key.

## Notes

- Fixtures: record real SSE streams once via `curl -N` (redact ids) into
  `crates/core/tests/fixtures/sse/*.txt`; keep them small.
- The adapter must not know about sessions, traces or tools' execution — only
  the wire protocol. That keeps it swappable for an OpenAI-compatible adapter
  in M10.

## Completion notes (2026-09-12)

- `apprentice_core::mentor`: `types` (wire types; `Role` includes `System`
  for mid-conversation system messages, needed by M03-08; `CacheFlag`
  flattens to `cache_control: {type: "ephemeral"}`; `ContentBlock` and
  `StopReason` have untagged fallbacks so newer API additions pass through),
  `sse` (in-house parser, chunk-boundary safe), `assemble` (block assembly,
  cumulative usage, `stop_details`), `anthropic` (`AnthropicMentor`:
  streaming over `reqwest` 0.13/rustls, retries with jittered exponential
  backoff capped at 60 s and honouring `retry-after`, cancellation via
  `CancellationToken` at both the request and every chunk, `count_tokens`),
  `error` (`MentorError` → RPC -32020/-32021/-32030 with
  `details {reason, http_status, api_error_type | retry_after_ms}`).
- Retry policy as specified: only before any content block started. An
  abrupt close before content is `Disconnected` (retryable, even though
  hyper reports it as a decode error); after content it is
  `StreamInterrupted { partial }`, including an SSE `error` event after
  content. `AnthropicMentor::with_max_backoff` caps delays (tests use 20 ms).
- `http_client(config)`: overall timeout from `mentor.timeout_s`, 90 s read
  timeout (the API pings while thinking), 10 s connect timeout,
  `apprentice-harness/<version>` user agent.
- Fixtures under `crates/core/tests/fixtures/sse/` are hand-written to the
  documented event shapes (not recorded from the API); the live test is
  what validates the real stream. Snapshot
  `tests/snapshots/mentor__messages_request.snap` was reviewed against the
  docs: system array with `cache_control`, tools with `cache_control`,
  `thinking {adaptive, display}`, `output_config.effort`, `stream: true`,
  thinking blocks echoed with `signature`, `tool_result` blocks, a
  `role: "system"` message, `metadata.user_id`, no assistant prefill.
- Tests: 16 unit (types, SSE, assembler, status mapping) + 13 integration
  (`crates/core/tests/mentor.rs`: text, parallel tool_use, thinking +
  redacted_thinking, cached usage, max_tokens, refusal with stop_details,
  error event before/after content, 429 + 529 then success, 401/400 not
  retried, rate-limit RPC mapping, count_tokens body, raw SSE capture,
  snapshot, cancellation mid-stream, disconnect before/after content over a
  raw socket server).
- Not run here: the `#[ignore]` live test (`HARNESS_LIVE=1
  ANTHROPIC_API_KEY=... cargo test -p apprentice-core --test mentor --
  --ignored live`) — it spends real tokens.
