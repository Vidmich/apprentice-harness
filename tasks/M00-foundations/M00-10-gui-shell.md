# M00-10 — GUI shell (Tauri) connected to the daemon

Status: todo
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

- [ ] Fresh machine flow: launch app → daemon auto-spawns → Setup asks for
      key → key stored → Playground runs a prompt → text streams → usage
      line shows tokens and cost.
- [ ] Killing the daemon while the app is open shows "disconnected" within
      2 s and reconnects/respawns automatically.
- [ ] Cancel stops streaming and the run shows status `cancelled`.
- [ ] Events for two concurrent runs (open two Playground tabs) do not
      cross (subscription routing is correct).
- [ ] `pnpm tauri build` produces an installer that includes the sidecar and
      works on a machine without `harnessd` on PATH.
- [ ] The API key is never written to disk by the GUI (search the app's data
      dirs after setup).

## Verification

Manual run-through on Windows; Vitest unit tests for `rpc.ts` type
mapping; a Rust unit test for event routing in the backend.

## Notes

- Tauri 2 sidecar binaries must be named
  `harnessd-<target-triple>[.exe]`; document in `apps/gui/README.md`.
- Keep the frontend free of business logic — anything that decides
  something belongs in the daemon so the CLI gets it too (SPEC parity rule).
