# M00-02 — JSON-RPC API types and local transport

Status: todo
Depends on: M00-01
Size: M

## Goal

`apprentice-api` defines the wire protocol between clients (CLI, GUI) and the
daemon: JSON-RPC 2.0 message types, method names, params/result structs,
notification (event) types, error codes. `apprentice-client` and the daemon
share a newline-delimited-JSON transport over a local socket, with a typed
client that supports request/response and subscriptions to event streams.

## Context

SPEC §3: "One core, thin clients" — GUI and CLI are both clients of the same
daemon API; feature parity is a checklist over this API. The API crate must
not depend on `core` so clients stay light.

## Scope

In: message framing, method registry, typed params/results for the M00
method set, event types, error model, client library, server-side dispatcher
scaffolding (used by M00-08), version handshake.
Out: implementing method behaviour (later tasks), remote/network transport.

## Design

### Framing and transport

- JSON-RPC 2.0 objects, one per line (`\n`-terminated, no pretty printing).
  Max line length 64 MiB (large tool results are referenced by blob id, not
  inlined, so real messages are far smaller).
- Local socket: Unix domain socket at `<data_dir>/daemon.sock`; on Windows a
  named pipe `\\.\pipe\apprentice-harness-<sha256(username)[..8]>`. Use the
  `interprocess` crate (`local_socket`, tokio feature) so both are one API.
- Optional `--stdio` transport for the daemon (stdin/stdout) for tests and
  embedding.
- Batch requests are not supported (reject with -32600).

### Handshake

First request on a connection must be `daemon.hello`:

```json
{"jsonrpc":"2.0","id":1,"method":"daemon.hello",
 "params":{"client":"harness-cli","client_version":"0.1.0","api_version":1,"token":"<from daemon.json>"}}
→ {"result":{"daemon_version":"0.1.0","api_version":1,"pid":1234}}
```

`token` is a random 32-byte hex string the daemon writes to
`<data_dir>/daemon.json` (see M00-08). Wrong token → error `-32001
unauthorized`. Mismatched `api_version` → `-32002 incompatible_api`.

### Method set for M00 (grows in later milestones)

| Method | Params | Result | Notes |
|---|---|---|---|
| `daemon.hello` | see above | see above | required first |
| `daemon.status` | – | `{version, pid, uptime_s, sessions_open, data_dir}` | |
| `daemon.shutdown` | `{graceful: bool}` | `{}` | |
| `config.get` | `{key?: string}` | `{value: json, source: "default"\|"user"\|"workspace"}` | dotted key; omitted = whole config |
| `config.set` | `{key, value: json, layer: "user"\|"workspace", workspace?: path}` | `{}` | |
| `config.path` | – | `{config_file, data_dir}` | |
| `auth.set_key` | `{provider:"anthropic", key}` | `{}` | stored in keychain |
| `auth.status` | – | `{providers:[{name, configured: bool}]}` | never returns the key |
| `session.create` | `{workspace?: path, title?}` | `{session_id}` | |
| `session.list` | `{limit?, offset?}` | `{sessions:[{id,title,workspace,created_at,updated_at}]}` | |
| `agent.run` | `{session_id, prompt, options?: {model?, effort?, apprentice?: bool}}` | `{agent_id, subscription}` | streams events |
| `agent.cancel` | `{agent_id}` | `{}` | |
| `trace.list` | `{session_id?, agent_id?, kinds?: [string], limit?, before_seq?}` | `{events:[EventSummary]}` | |
| `trace.get` | `{event_id, include_blob?: bool}` | `{event: Event, blob?: string\|null}` | |
| `stats.tokens` | `{since?: ts, until?: ts, session_id?}` | `TokenStats` | see M00-07 |

### Events (notifications)

Method `event`, params `{subscription: string, seq: u64, event: Event}`.
`Event` is a tagged enum (`"type"` field):

```
agent.started {agent_id, session_id}
agent.text_delta {agent_id, text}                  streamed assistant text
agent.thinking_delta {agent_id, text}              summarized thinking, if displayed
agent.tool_call {agent_id, call_id, name, input}   (M01)
agent.tool_result {agent_id, call_id, ok, summary, blob_id?}   (M01)
agent.usage {agent_id, call_id, usage: Usage}      per mentor call
agent.finished {agent_id, status: "ok"|"cancelled"|"error", error?: RpcError}
permission.request {request_id, agent_id, tool, input, risk}   (M01)
log {level, message}                               optional, for GUI console
```

A subscription ends with `agent.finished`; the daemon then stops sending for
that subscription id. Clients must tolerate unknown event types (skip).

### Errors

`RpcError { code: i32, message: String, data: { kind: String, details?: json } }`.
Reserved: -32700 parse, -32600 invalid request, -32601 method not found,
-32602 invalid params, -32603 internal. Application codes:
-32001 unauthorized, -32002 incompatible_api, -32010 not_found,
-32011 conflict, -32020 mentor_error (details: http_status, api_error_type),
-32021 mentor_rate_limited (details: retry_after_ms), -32030 cancelled,
-32040 permission_denied, -32050 config_error.

### Crate `apprentice-api`

```
src/lib.rs        API_VERSION: u32 = 1
src/jsonrpc.rs    Request, Response, Notification, RpcError, Id (num|string)
src/methods.rs    one struct pair per method: `XParams`, `XResult`; trait `Method { const NAME: &str; type Params; type Result; }`
src/events.rs     Event enum, Usage struct
src/types.rs      shared: SessionSummary, EventSummary, TokenStats, Effort enum, ...
```

All types `Serialize + Deserialize + Debug + Clone`, `#[serde(rename_all = "snake_case")]`,
unknown fields ignored on input (forward compatibility), `#[non_exhaustive]`
on enums that will grow.

### Crate `apprentice-client`

```
DaemonClient::connect(opts) -> Result<Self>      opts: {spawn_if_missing: bool, home: Option<PathBuf>, timeout}
client.call::<M: Method>(params) -> Result<M::Result>
client.subscribe(subscription_id) -> impl Stream<Item = Event>
client.call_streaming::<M>(params) -> (M::Result, Stream<Event>)   for agent.run
```

Implementation: one tokio task reads lines and routes responses by id to
oneshot channels and notifications to per-subscription mpsc channels
(bounded 1024; on overflow, drop with a warning event). Discovery/spawn logic
lives in M00-08; here only the `connect(path)` primitive.

### Server-side dispatcher (used by the daemon)

`apprentice_api::server::Router` maps method names to async handlers
`Fn(Params) -> Result<Value, RpcError>` with a `Connection` handle that can
`notify(subscription, event)`. Connection state stores `hello`-authenticated
flag. Put the router in `apprentice-api` (no core dependency) so it can be
unit-tested with in-memory duplex streams (`tokio::io::duplex`).

## Acceptance

- [ ] All M00 methods and events have typed structs with serde round-trip
      tests (serialize → deserialize equals original) and a snapshot test of
      the JSON shape (`insta`).
- [ ] Router rejects any method before `daemon.hello`, rejects wrong token,
      rejects `api_version` mismatch, returns -32601 for unknown methods and
      -32602 for malformed params.
- [ ] Client handles: concurrent requests with interleaved responses,
      notifications for two subscriptions interleaved, server closing the
      connection mid-request (error, not hang), lines over the max size.
- [ ] Transport works on Windows named pipe and Unix socket (test on the
      current OS; the other path is covered by CI later).
- [ ] `--stdio` transport works end to end with the same router.

## Verification

`cargo test -p apprentice-api -p apprentice-client`. Include a test that spins
a router on a `tokio::io::duplex` pair, connects a `DaemonClient` over it,
performs hello + `daemon.status` + a fake streaming method that emits 3 events
and a finish.

## Notes

- Keep `Usage` identical to what the mentor returns:
  `{input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens}`
  (all u64, missing → 0). Cost is computed server-side (M00-07), never by
  clients.
- Method names are `<area>.<verb>`; adding a method later must not change
  existing shapes (additive only, bump `API_VERSION` on breaking change).
