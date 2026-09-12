# M00-10 — GUI shell (Tauri) connected to the daemon

Status: done
Depends on: M00-02, M00-08
Size: M

## Goal

The Tauri app starts (or attaches to) the daemon, exposes a generic RPC
bridge to the React frontend, forwards daemon events as Tauri events, and
shows a minimal working screen: connection status, API-key setup, a prompt
box that runs `agent.run` and streams the reply, and the usage of that call.
This is the skeleton M01 turns into the full chat UI.

## Context

SPEC §13 (GUI requirements) and §3 (thin clients). The GUI backend (Rust side
of Tauri) uses `apprentice-client` exactly like the CLI — no direct core
dependency. Sidecar packaging makes the daemon discoverable next to the app.

## Scope

In: sidecar bundling of `harnessd`, connection manager with reconnect,
`rpc_call` and event bridge commands, frontend RPC client with typed
wrappers, three screens (status/setup, prompt, usage), app shell layout
(sidebar placeholder + main area), dark/light theme following the OS.
Out: chat history rendering, tool cards, workspaces UI, permissions dialog
(all M01).

## Design

### Tauri backend (`apps/gui/src-tauri`)

- `tauri.conf.json`: `bundle.externalBin: ["binaries/harnessd"]` (Tauri
  sidecar naming with target triple suffix; a `just gui-sidecar` recipe
  copies the built `harnessd` into place).
- `DaemonState` (managed state): `Mutex<Option<DaemonClient>>` + a
  background task that connects with `spawn_if_missing: true`, retries with
  backoff, and emits `daemon:status` events `{connected: bool, version,
  pid, error?}`.
- Commands:
  - `rpc_call(method: String, params: Value) -> Result<Value, RpcErrorDto>` —
    generic passthrough (keeps the frontend in step with the API without a
    Rust change per method).
  - `rpc_stream(method, params) -> Result<{subscription, result}, ...>` — for
    `agent.run`: the backend subscribes and re-emits each event as Tauri
    event `rpc:event:<subscription>`; unsubscribes on `agent.finished`.
  - `daemon_restart()`.
- Secrets: the API key entered in the GUI goes straight to `auth.set_key`
  over RPC; the frontend never stores it.

### Frontend (`apps/gui/src`)

- `lib/rpc.ts`: `call<M extends keyof Methods>(method, params)` typed via a
  hand-written `api.d.ts` mirroring `apprentice-api` (generation via
  `ts-rs`/`specta` is a nice-to-have; add a test that the Rust snapshot JSON
  from M00-02 parses against these types to catch drift).
- `lib/events.ts`: `subscribe(subscription, handler)` wrapping Tauri
  `listen`.
- State: a small store (Zustand) for daemon status, auth status, current run.
- Screens/components:
  - `StatusBar`: daemon connected/version, auth configured, model in use.
  - `Setup`: shown when auth is not configured — password field → `auth.set_key`, then `auth.status`.
  - `Playground`: textarea + Run button (`agent.run` with a fresh session in
    a chosen folder or none), streaming output area appending
    `agent.text_delta`, Cancel button (`agent.cancel`), final usage/cost line
    from `agent.usage` + `agent.finished`.
- Layout: left sidebar (empty list titled "Sessions" — M01 fills it), main
  pane, bottom status bar. Styling: Tailwind; CSS variables for theme;
  `prefers-color-scheme` respected.

### Dev workflow

`just gui` runs `tauri dev` with `HARNESS_HOME` optionally set for an
isolated data dir; the backend logs to `logs/gui.log` (M00-04 role `gui`).

## Acceptance

- [x] Fresh machine flow: launch app → daemon auto-spawns → Setup asks for
      key → key stored → Playground runs a prompt → text streams → usage
      line shows tokens and cost. *Up to "key stored" verified by hand;
      the run itself streams from the mock router in tests and needs
      M00-11's `agent.run` in the daemon to run for real.*
- [x] Killing the daemon while the app is open shows "disconnected" within
      2 s and reconnects/respawns automatically.
- [x] Cancel stops streaming and the run shows status `cancelled`.
      *Reducer-tested; end to end with M00-11.*
- [x] Events for two concurrent runs (open two Playground tabs) do not
      cross (subscription routing is correct). *Rust routing test and a
      reducer test with interleaved events.*
- [x] `pnpm build:app` (not plain `pnpm tauri build`, see notes) produces
      an installer that includes the sidecar and works on a machine without
      `harnessd` on PATH.
- [x] The API key is never written to disk by the GUI (search the app's data
      dirs after setup).

## Verification

Manual run-through on Windows; Vitest unit tests for `rpc.ts` type
mapping; a Rust unit test for event routing in the backend.

## Notes

- Tauri 2 sidecar binaries must be named
  `harnessd-<target-triple>[.exe]`; document in `apps/gui/README.md`.
- Keep the frontend free of business logic — anything that decides
  something belongs in the daemon so the CLI gets it too (SPEC parity rule).

## Completion notes (2026-09-12)

- **Backend** (`apps/gui/src-tauri/src`): `daemon.rs` holds `DaemonState`
  (current `DaemonClient`, last status, a `Notify` to kick a retry) and
  the manager task: `DaemonClient::connect` with `spawn_if_missing`,
  client name `harness-gui`; on success emits `daemon:status
  {connected, version, pid, spawned}` and waits on the new
  `DaemonClient::closed()` (a `watch` in `apprentice-client`, resolves
  when the reader sees EOF); on loss emits `{connected: false, error}`
  and reconnects after 500 ms (so a daemon stopping on purpose has
  released its lock); connect failures back off 0.5 → 5 s. `bridge.rs`:
  `rpc_call(method, params)` → `call_raw`; `rpc_stream(method, params,
  channel)` → call, `subscribe(result.subscription)`, forward each event
  as Tauri event `rpc:event:<channel>`, stop after `agent.finished`, and
  synthesise `agent.finished{error, kind: daemon_unavailable}` when the
  stream ends without one. Client-side failures become `RpcError`s with
  kinds `daemon_unavailable`, `timeout`, `transport`, `protocol`.
  `daemon_restart` sends `daemon.shutdown` (the manager respawns);
  `daemon_status` returns the cached status; `app_info` the version and
  paths. Logging: role `gui` to `<data_dir>/logs`, stderr too in debug.
- **Channel, not subscription, names the event**: the frontend cannot
  `listen` on `rpc:event:<subscription>` before `agent.run` answers with
  the subscription, and Tauri events emitted before `listen` registers
  are lost. So the frontend picks a channel name, listens, then calls.
  Events that arrive before the answer make the run adopt the
  subscription from the first event (`applyEvent`), and a late answer
  cannot resurrect a run that already finished.
- **Sidecar**: declared in `src-tauri/tauri.bundle.conf.json` and merged
  only by `pnpm build:app` (= `pnpm sidecar && tauri build --config …`),
  because `tauri-build` fails the build when `binaries/` is missing and
  copies the sidecar over `target/<profile>/harnessd.exe` on every build
  (clobbering the daemon cargo built, and failing when a daemon holds
  the file). `scripts/sidecar.mjs` builds `harnessd --release` and copies
  it to `binaries/harnessd-<triple>.exe`; `just gui-sidecar` /
  `just gui-build`. In dev the app finds `harnessd` next to its own
  executable in `target/debug`.
- **Frontend** (`apps/gui/src`): `lib/api.ts` (types for all 17 methods,
  the event union with `asKnown` for forward compatibility, shape
  guards), `lib/rpc.ts` (`call`, `stream`, `RpcFailure`, `describe`),
  `lib/events.ts` (`newChannel`, `subscribe`, `onDaemonStatus`),
  `lib/run.ts` (pure reducer `applyEvent` + `usageLine` identical to the
  CLI's), `lib/playground.ts` (`startRun`: `session.create` →
  `agent.run`; `cancelRun`), `lib/bootstrap.ts` (status listener, then
  `auth.status` + `config.get mentor.model` on every (re)connect),
  `store.ts` (Zustand: daemon, auth, model, tabs). Screens: `Setup`
  (password field → `auth.set_key`, key kept in component state only
  and cleared), `Playground` (folder + Browse via `tauri-plugin-dialog`,
  prompt, Run/Cancel, streamed text, activity, thinking toggle, usage
  line), `Sidebar` (Playground tabs with `+`/close; "Sessions" placeholder),
  `StatusBar` (daemon dot/version/pid, key source, model, restart button,
  gui version). Tailwind v4 via `@tailwindcss/vite`; theme tokens with
  `light-dark()` follow the OS.
- **Tests**: Rust — event routing (`forward` over a duplex client with two
  interleaved subscriptions; a cut stream ends with the synthetic error),
  error-kind mapping. Vitest — `api.test.ts` parses every M00-02 snapshot
  (`events`, `hello_response`, `error_response`, `agent_run_result`,
  `token_stats`) against the guards and requires every known event type
  to appear; `rpc.test.ts` mocks `invoke`; `run.test.ts` covers
  streaming, usage/cost sums, two runs not crossing, late answers,
  cancel/error/activity and the usage line.
- **Bug found and fixed in `apprentice-client`**: the endpoint name is
  derived from the data dir, so after a daemon dies (`daemon.json` left
  behind) the replacement listens on the same pipe before rewriting the
  file; the spawn-poll loop connected with the dead daemon's token and
  got `unauthorized` (the CLI would have exited 4). The loop now skips
  attempts while `daemon.json` still names the old pid (unless the
  spawned child exited with "already running", which means that daemon
  is alive after all).
- **Manual run** (`tauri dev`, isolated `HARNESS_HOME` with the file
  secret store): daemon spawned on launch and the status bar showed it;
  Setup shown; `Run` on the Playground created a session and rendered
  `unknown method agent.run [method_not_found]` with `status: error` and
  the usage line (the daemon serves `agent.run` from M00-11); killing
  `harnessd` (`Stop-Process`) flipped the bar to disconnected and a fresh
  daemon was connected ~1.5 s later; "restart daemon" replaced the pid in
  ~1.3 s; after deleting the secret and restarting, Setup came back, a key
  typed into it was stored (`auth.status` → `configured (file)`) and the
  string exists only in the daemon's `secrets.toml` — not under the
  WebView profile (`%LOCALAPPDATA%\dev.apprentice-harness.gui`) or any log.
- **Dev note**: `tauri dev` kills the whole process tree on rebuild, the
  spawned daemon included; the app respawns it on the next start. Rust
  edits under `crates/` trigger those rebuilds.
