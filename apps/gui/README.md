# apprentice-harness GUI

Tauri 2 + React + Vite + TypeScript. The Rust side (`src-tauri`) is a thin
client of the daemon through `apprentice-client`; it never links
`apprentice-core`.

## Prerequisites (Windows)

- Rust stable (see `../../rust-toolchain.toml`), MSVC build tools
- WebView2 runtime (bundled with Windows 11)
- Node 22+, pnpm 10+

## Commands

```
pnpm install
pnpm tauri dev      # dev window with HMR (or `just gui` from the repo root)
pnpm build:app      # installer with the daemon sidecar (or `just gui-build`)
pnpm lint / pnpm test / pnpm typecheck
```

Set `HARNESS_HOME` to run against an isolated config/data directory. The
backend logs to `<data_dir>/logs/gui.<date>.log` (role `gui`); the log
level comes from `HARNESS_LOG_LEVEL`. In debug builds the log also goes to
the terminal that started `tauri dev`.

`tauri dev` rebuilds and restarts the app when Rust sources change
(including `crates/`), killing the whole process tree — the daemon it
spawned included; the restarted app spawns a fresh one.

## How it is wired

- `src-tauri/src/daemon.rs` — connection manager. Connects through
  `daemon.json`, spawning `harnessd` when none runs (found next to the app
  executable, then `HARNESS_DAEMON_PATH`, then `PATH`), reconnects with
  backoff when the connection drops, and emits `daemon:status`
  (`{connected, version?, pid?, error?, spawned}`) on every change.
- `src-tauri/src/bridge.rs` — generic commands: `rpc_call(method, params)`
  passes any daemon method through untyped; `rpc_stream` takes the same
  plus a `channel`, calls a streaming method and re-emits each of its
  events as the Tauri event `rpc:event:<channel>` until `agent.finished`. The frontend
  picks the channel name and listens before calling, so no event is lost.
  A connection that drops mid-stream produces a synthetic
  `agent.finished{status: error, kind: daemon_unavailable}`.
- `daemon_restart` asks the daemon to stop; the manager respawns it.
- `src/lib/api.ts` — hand-written types mirroring `crates/api`. `api.test.ts`
  parses the Rust snapshot JSON (`crates/api/tests/snapshots`) against
  them, so a wire change on the Rust side fails the GUI tests.
- `src/lib/rpc.ts` (`call`, `stream`), `src/lib/events.ts` (`subscribe`,
  `onDaemonStatus`), `src/lib/run.ts` (the pure reducer behind the
  Playground), `src/store.ts` (Zustand).

The frontend holds no business logic and never stores the API key: the
Setup screen sends it to `auth.set_key` and forgets it.

## Sidecar

`harnessd` ships inside the installer as a Tauri sidecar. Tauri requires
the file to be named `src-tauri/binaries/harnessd-<target-triple>[.exe]`
(e.g. `harnessd-x86_64-pc-windows-msvc.exe`); `pnpm sidecar` (or
`just gui-sidecar`) builds the release daemon and copies it there, and
`pnpm build:app` runs that before `tauri build`.

The sidecar is declared in `src-tauri/tauri.bundle.conf.json`, merged in
only for `build:app`, rather than in `tauri.conf.json`: with `externalBin`
in the base config `tauri-build` fails whenever `binaries/` is missing
(every fresh clone, CI, plain `cargo build --workspace`) and copies the
sidecar over `target/<profile>/harnessd.exe` on each build, clobbering the
daemon cargo just built. In development the app finds `harnessd` next to
its own executable in `target/debug` anyway.
