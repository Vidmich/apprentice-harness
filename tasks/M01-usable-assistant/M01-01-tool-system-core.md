# M01-01 — Tool system core

Status: done
Depends on: M00-06, M00-11
Size: M

## Goal

`apprentice_core::tools` defines how tools are declared, registered,
validated, executed, limited and recorded, independent of any particular
tool. Concrete coding tools (M01-03..06) plug into it; the runtime (M01-08)
drives it; the permission engine (M01-07) gates it.

## Context

SPEC §3.1 Tool system: JSON-schema-declared tools with a risk class,
executable by the mentor now and by the apprentice later (Executor role,
M07); productivity tools must be addable later without runtime changes.
Every call and result is a trace event (SPEC §9).

## Scope

In: `Tool` trait, registry, schema validation, risk classes, execution
context, timeouts, output limits and truncation policy, cancellation,
result summaries, trace recording, RPC `tools.list`.
Out: any specific tool; permission decisions (M01-07); apprentice
compression of results (M03).

## Design

### Trait and registry

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;                                   // name, description, input_schema (JSON Schema draft 2020-12), risk, tags
    async fn call(&self, ctx: &ToolContext, input: Value, cancel: CancellationToken) -> Result<ToolOutput, ToolError>;
}
pub struct ToolSpec { pub name: String, pub description: String, pub input_schema: Value, pub risk: Risk, pub tags: Vec<String> }
pub enum Risk { ReadOnly, Write, Execute, Network }
pub struct ToolContext { pub workspace: Arc<Workspace>, pub session_id, pub agent_id, pub call_id, pub env: ToolEnv /* timeouts, limits from config */ , pub progress: mpsc::Sender<ToolProgress> }
pub struct ToolOutput { pub content: ToolContent /* Text(String) | Json(Value) | Binary{media_type, bytes} */, pub summary: String, pub metadata: Value, pub is_error: bool }
pub enum ToolError { InvalidInput(String), Denied(String), Timeout, Cancelled, Io(std::io::Error), Failed(String) }
pub struct ToolRegistry { tools: BTreeMap<String, Arc<dyn Tool>> }   // BTreeMap → deterministic order for cache stability
```

Names: `snake_case`, unique. `description` is the text the mentor sees —
write it for the mentor (what it does, when to use it, argument semantics),
not for humans.

### Execution wrapper (`tools::execute`)

Order of operations for one call:
1. Validate `input` against `input_schema` (`jsonschema` crate); failure →
   `ToolError::InvalidInput` returned to the mentor as `tool_result{is_error}`.
2. Record `tool.call` event (input blob if large).
3. Ask the permission engine (M01-07) — may block on the user; denial →
   error result `is_error: true` with text "denied by user".
4. Run with `tokio::time::timeout(ctx.env.timeout)`; default timeouts by
   risk: ReadOnly 30 s, Write 30 s, Execute 600 s, Network 120 s; per-tool
   override in spec metadata.
5. Apply output limits: `max_output_bytes` default 256 KiB for the trace blob
   (store whole output up to 8 MiB; beyond that store a head+tail and mark
   `truncated_at_capture`) and `max_mentor_bytes` default 32 KiB for what the
   mentor receives in M01 (head 24 KiB + tail 8 KiB with a
   `[... N bytes omitted, full result id <blob-id>]` marker). The apprentice
   compressor replaces this crude policy in M03; the *raw* blob is what it
   will train on, so capture it fully.
6. Record `tool.result` event with `{ok, duration_ms, output_bytes,
   truncated, summary}` and the raw output blob.
7. Return the `ContentBlock::ToolResult` to build into the next mentor
   request.

Parallel tool calls: the runtime executes all `tool_use` blocks of one
assistant message concurrently (`join_all`) unless any has
`Risk::Write|Execute`, in which case those run sequentially in order after
the read-only ones. All results go back in one user message.

### Summaries

Every `ToolOutput.summary` is one line ≤ 120 chars for the GUI/CLI card and
for `tool.result.summary` ("read 212 lines of src/main.rs", "grep: 14
matches in 6 files", "exit 1 in 3.2 s"). Tools produce it; the wrapper
falls back to `"<name>: <bytes> bytes"`.

### RPC

`tools.list` → `{tools: [ToolSpec + enabled: bool]}`. Enabling/disabling
per workspace is a config key `tools.disabled = ["shell"]`.

## Acceptance

- [x] A `MockTool` in tests exercises: schema failure → error result;
      timeout → error result and `tool.result{ok:false}`; cancel →
      `Cancelled`; large output → blob stored fully, mentor text truncated
      with marker; summary present. *`crates/core/tests/tools.rs`; also
      unknown tool, gate denial, `is_error` results, JSON and binary
      content, the capture limit, summary normalisation.*
- [x] Registry serialises tool defs deterministically (sorted by name) —
      snapshot test. *`tools__tool_defs.snap`, `tools__tools_list.snap`.*
- [x] Parallel policy test: 2 read-only + 1 write call → read-only run
      concurrently, write runs after; results returned in original order.
      *Plus an `Execute` call, which runs after the write.*
- [x] `tools.list` reflects `tools.disabled` from workspace config.
      *Through `ToolsService` with a user and a workspace `config.toml`;
      `harness tools list` checked against a live daemon (empty list).*

## Verification

`cargo test -p apprentice-core tools::`.

## Notes

- The `progress` channel lets long tools (shell) stream partial output to
  the GUI (`agent.tool_progress` event added in M01-08); not required for
  read/write tools.
- Keep `ToolContent::Binary` for future screenshots (vision role, M10).

## Completion notes (2026-09-12)

- **Module** `crates/core/src/tools/`: `mod.rs` (the `Tool` trait,
  `ToolSpec`, `ToolContext`, `ToolEnv`, `ToolOutput`/`ToolContent`,
  `ToolError`, `ToolProgress`), `schema.rs` (name rules, compiled
  validators), `registry.rs`, `limits.rs` (head + tail cuts on char
  boundaries, one-line summaries), `execute.rs` (the wrapper and the
  batch policy), `rpc.rs` (`tools.list`). `Risk` is the one already in
  `apprentice-api` (events carry it), re-exported.
- **Deviations from the sketch.** `ToolContext.workspace` is
  `Option<Arc<PathBuf>>` (the root) until M01-02 provides `Workspace`.
  `ToolSpec` carries `timeout_s` (the per-tool override) instead of a
  metadata bag. Step 2 of the wrapper (record `tool.call`) runs before
  step 1 (validate) so the trace also has calls that never ran — an
  unknown name or a schema violation is what the mentor did, and the
  apprentice will learn from it. The permission step is a `Gate` trait
  (`permit(ctx, spec, input) -> Result<(), reason>`), `AllowAll` until
  M01-07; `reason` is the text the mentor reads, so "denied by user" is
  the engine's to say. Tool ids are the mentor's `tool_use` ids, not
  generated `CallId`s: the `tool.call`/`tool.result` events carry them as
  `call_id` and the result block echoes them.
- **Trace payloads.** `tool.call {call_id, name, risk, input_hash,
  input_bytes}` + the input JSON as blob (always; content-addressed, so
  small inputs cost one row). `tool.result {call_id, name, ok, kind,
  duration_ms, output_bytes, mentor_bytes, truncated, summary,
  media_type?, message?, metadata?, risk?, truncated_at_capture?}` + the
  raw output as blob. `kind` is `ok | error | invalid_input | denied |
  timeout | cancelled | failed`; `ok` is `kind == ok`. `is_error`
  outputs are captured like successes (the diagnostic is the useful
  part); wrapper errors have no blob and put their message in `message`.
- **Limits.** New `[tools]` config section (workspace-overridable):
  `disabled`, `max_capture_bytes` (8 MiB), `max_mentor_bytes` (32 KiB),
  `timeout_s.{read_only,write,execute,network}` (30/30/600/120). The
  mentor's copy is 3/4 head + 1/4 tail around
  `[... N bytes omitted, full result id <blob>]`; the capture cut uses
  the same shape and records the real `output_bytes`. Both cuts respect
  UTF-8 boundaries. Binary content reaches the mentor as a one-line
  placeholder naming media type, size and blob (vision is M10). JSON
  content is pretty-printed for the mentor, stored compact.
- **Timeouts and cancellation.** The tool gets a child token; on the
  agent token or the timeout the wrapper cancels the child and drops the
  future, so a tool that ignores its token is still abandoned. A call
  that starts after cancellation is recorded as `cancelled` without
  running. The 1 s floor on timeouts keeps `timeout_s = 0` from meaning
  "never".
- **Policy.** `execute_all` partitions by `is_mutating` (`Write` and
  `Execute`); the rest, including unknown names, run in a `join_all`,
  then the mutating calls in order. `tool.call` rows of the concurrent
  phase can interleave; results keep the mentor's order.
- **Wire.** `tools.list {workspace?} → {tools: [ToolInfo + enabled]}` in
  `apprentice-api` (snapshot `tools_list`), `api.ts` and the vitest
  parity test (18 methods), `harness tools list [--workspace DIR]
  [--describe]`. `AppState` owns an `Arc<ToolRegistry>` (`tools()`),
  empty until M01-03 registers the file tools.
- **Dependency.** `jsonschema` 0.56 without default features (no HTTP
  resolver, no TLS); draft 2020-12, formats validated. `cargo deny`
  stays clean.
- Open: the runtime does not call any of this yet (`build_request` still
  sends no tools) — M01-08 wires `registry.defs(&config.tools.disabled)`
  into the request, the executor into the loop and `ToolProgress` into
  `agent.tool_progress`.
