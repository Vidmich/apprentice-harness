# M02-09 — Remote inference endpoint (optional)

Status: todo
Depends on: M02-04, M02-05, M02-06
Size: M

## Goal

The same inference service exposed by a `harnessd` on a second machine
over the network (`harnessd --serve-inference <bind>`), and consumed by
the local daemon as a drop-in backend (`inference.remote = "host:port"`)
with token authentication, so a user with a separate GPU box keeps the
whole apprentice experience — generation, encoders, session states,
metrics — while the laptop runs the GUI and the tools. Deferred if M02
runs short; nothing else in M02 depends on it (M10 lists it again).

## Context

SPEC §7 ("Optional: use a second machine's service over the network (same
API), for users with a separate GPU box"), §15 (all data local by
default; nothing leaves the machine except mentor API calls and
explicitly triggered exports — a remote inference host is an explicit
choice, and prompts are the user's own data going to the user's own
machine), §3 ("Core runs as a daemon"). M00-02's JSON-RPC framing and
router are reused over TCP; M02-04's command channel is the seam.

## Scope

In: the server mode, the wire methods, token auth, the client backend
implementing the service's `Backend` trait over the wire, streaming,
reconnection, metrics pass-through, config, CLI, an E2E over loopback.
Out: TLS (a documented `ssh -L` / WireGuard recommendation instead — a
LAN-only feature in M02), multi-user sharing of one GPU box, auto-
discovery, remote model management beyond listing (pull on the remote
via its own CLI).

## Design

### Server mode

```
harnessd --serve-inference 0.0.0.0:7431 [--home DIR]
```

Runs the normal daemon (its own store, config, models, service) plus a
TCP listener with the M00-02 newline-JSON framing and a **separate
router** exposing only the inference methods; every connection must
open with `inference.hello {token, api_version, client}` — the token is
`inference.serve_token` from the remote's secret store (`harness auth
set-key --provider inference-serve`, or generated on first `--serve-
inference` and printed once). Wrong token → close after `-32001`. The
local socket / named pipe stays for the remote machine's own CLI/GUI.
Rate: one `Interactive` request per connection in flight is *not*
enforced — the remote's admission control (M02-05) is the limiter, as
for local callers.

### Wire methods (additive to `apprentice-api`)

`inference.hello`, `inference.status`, `inference.generate {req} →
{subscription}` streaming `inference.token {text|token}` and
`inference.done {usage | error}`, `inference.cancel {subscription}`,
`inference.tokenize`, `inference.encode/score/classify`, `inference.
state_open/append/generate/snapshot/restore/fork/drop` (M02-04b's trait,
one method each; snapshot bytes as base64 in a blob-sized message — the
64 MiB line limit of M00-02 caps a snapshot at ~48 MB, so larger ones
are chunked: `state_snapshot` returns `{chunks: n}` and `state_snapshot_
chunk {i}` fetches each), `models.list` (read-only view of the remote's
store; assignment is the *local* config's business and names remote
ids), `inference.metrics_reset`.

The local `apprentice.invocation` record is written by the local
service with `remote: "host:port"` and the remote's `Usage` (queue,
prefill, decode) plus the measured round-trip overhead
(`transport_ms`), so the bench and stats see the network cost.

### Client backend

`inference::backend::remote::RemoteBackend` implements the same
`Backend` trait as the llama and mock backends (M02-04): the local
scheduler thread is replaced by a connection task that multiplexes
requests over one persistent TCP connection (a second one for
`Background` traffic so a big append never queues an interactive
compression behind it), reconnects with backoff on loss (in-flight
requests fail with `Bypassed{Unavailable}`; the caller falls back to the
mentor as for any bypass), and answers `status` from the remote's plus
the connection state. Admission (M02-05) runs on the remote; the local
side adds `transport_ms_ewma` to the ETA it passes through so budgets
stay honest. Session states (M02-04b) live on the remote; the local
`StateManager` proxies handles and keeps snapshots in the *local* trace
(the bytes cross the wire once per snapshot).

`inference.remote = ""` (default) → local backend; a value → remote; the
service refuses to have both and reports which one it uses in
`inference.status {backend: local|remote, endpoint}`.

### Config and secrets

```toml
[inference]
remote = "gpubox.lan:7431"        # local side
serve_token_required = true       # remote side; false only for loopback tests
serve_max_connections = 8
```

Secrets: `inference_remote_token` (local side, `harness auth set-key
--provider inference-remote`), `inference_serve_token` (remote side).
Never in config files.

### Surfaces

- CLI: `harness apprentice remote status|test` (hello + a 16-token
  generation with timings), `harness daemon start --serve-inference
  BIND`.
- GUI (M02-08 hook): the service panel shows `remote: host` and the
  transport latency; no other UI in M02.

## Acceptance

- [ ] Loopback E2E (`crates/daemon/tests/remote_inference.rs`): a
      `harnessd --serve-inference 127.0.0.1:0` with the mock backend
      (`HARNESS_INFERENCE_BACKEND=mock`) and a second `harnessd` with
      `inference.remote` pointed at it: `harness apprentice run` streams
      tokens through both; the local trace has the record with `remote`
      and `transport_ms`; wrong token → `unauthorized`; killing the
      remote mid-generation → `Bypassed{Unavailable}` locally within
      2 s and a reconnect once it is back.
- [ ] Encoders and session states work over the wire (score a pair;
      open a state, append, snapshot (chunked when > 48 MB in a mock
      with a large fake state), restore on the remote after a reconnect
      from the local trace's blob).
- [ ] M02-07's bench with `inference.remote` set reports `transport_ms`
      per role and the same pass/fail logic applies; on the LAN between
      the reference machine and a second box (or two daemons on one
      machine with a `tc`/`clumsy`-injected 5 ms delay) the compressor
      p95 stays within budget.
- [ ] `inference.status` on the local side shows `backend: remote`, the
      endpoint and the connection state; `harness apprentice remote test`
      prints hello, model, and the round-trip timings.
- [ ] The remote's own CLI/GUI keep working on its local socket while it
      serves.

## Verification

`cargo test -p harnessd --test remote_inference` (loopback, mock
backend); a manual LAN run documented in the completion notes with the
transport latency measured.

## Notes

- Plain TCP with a bearer token is acceptable for a LAN behind the user's
  own router and is what this task ships; the README of the feature says
  so and recommends an SSH tunnel or WireGuard for anything else. TLS
  with a self-signed cert is the obvious M10 follow-up if demand exists.
- Keep the remote router a strict subset: nothing that reads the
  remote's traces, sessions or config is exposed.
