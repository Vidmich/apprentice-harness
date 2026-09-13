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

`pnpm dev` alone serves the page to a plain browser at
`http://localhost:1420`. Outside Tauri the page talks to the scripted
mock daemon in `src/lib/mock.ts` instead of `harnessd`: sessions,
`agent.run` with a canned streamed answer (thinking, markdown, a read,
an edit with a diff, a shell with streamed output), cancel, reattach
after a reload, and the trace events behind the Raw tab. Words in the
prompt steer it: `fail` (an error before any tool, for Retry), `slow`
(a minute-long shell, for Cancel and reload), `big` (5 MB of shell
output), `warn` (a warning in the footer). It costs no tokens and is
where the chat view's rendering is worked on; the real daemon is only
reachable from the app. `TESTING.md` is the manual checklist for both.

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
- `src/lib/bridge.ts` — `invoke`/`listen`: Tauri's in the app, the mock's
  in a browser. `src/lib/rpc.ts` (`call`, `stream`) and `src/lib/events.ts`
  (`subscribe`, `onDaemonStatus`) sit on it.
- `src/lib/transcript.ts` — the pure reducer behind the chat view: the
  stored rows of `session.get` plus a live overlay folded from the run's
  events, derived into a flat item list (user, assistant text, thinking,
  tool card, turn footer, error). `transcript.test.ts` covers it.
- `src/lib/chat.ts` — the effects: open a session (newest page first,
  older pages on scroll), send (`agent.run`), cancel, reattach to a run
  after a reload (`agent.subscribe`; the stored rows fill the gap at the
  next step boundary), refresh at step boundaries, and the raw output of
  a tool call from the trace (`trace.list` + `trace.get`). Events are
  folded in batches so a fast stream costs one render per frame.
- `src/store.ts` (app state, the chats — persisted in `localStorage` so a
  reload finds its sessions — and the composer settings),
  `src/stores/transcripts.ts` (one transcript per session, the five most
  recent kept).
- `src/components/chat/` — `TranscriptView` (sticks to the bottom,
  virtualises above 200 items), `Markdown` + `CodeBlock` (react-markdown,
  Shiki loaded on first use, both themes as CSS variables), `ToolCard`
  (Input / Result / Raw / Diff / Console tabs), `DiffView`, `RawView`
  (virtualised lines, megabytes are fine), `Console`, `Composer`.

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
