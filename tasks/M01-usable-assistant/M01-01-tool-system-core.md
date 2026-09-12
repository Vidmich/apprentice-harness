# M01-01 — Tool system core

Status: todo
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

- [ ] A `MockTool` in tests exercises: schema failure → error result;
      timeout → error result and `tool.result{ok:false}`; cancel →
      `Cancelled`; large output → blob stored fully, mentor text truncated
      with marker; summary present.
- [ ] Registry serialises tool defs deterministically (sorted by name) —
      snapshot test.
- [ ] Parallel policy test: 2 read-only + 1 write call → read-only run
      concurrently, write runs after; results returned in original order.
- [ ] `tools.list` reflects `tools.disabled` from workspace config.

## Verification

`cargo test -p apprentice-core tools::`.

## Notes

- The `progress` channel lets long tools (shell) stream partial output to
  the GUI (`agent.tool_progress` event added in M01-08); not required for
  read/write tools.
- Keep `ToolContent::Binary` for future screenshots (vision role, M10).
