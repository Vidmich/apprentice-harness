# M00-08 — Daemon lifecycle and client discovery

Status: done
Depends on: M00-02, M00-03, M00-04, M00-06
Size: M

## Goal

`harnessd` runs as a single per-user background process hosting the core
(config, trace store, mentor adapter, later inference). It publishes its
endpoint, enforces single-instance, shuts down gracefully, and can be
auto-spawned by any client. `apprentice-client` discovers it, spawns it when
absent, and reconnects after restarts.

## Context

SPEC §3 "Core runs as a daemon": one process shares the inference service and
trace store between GUI and CLI. M00-02 defines the wire protocol; this task
provides the process around it.

## Scope

In: startup/shutdown, lock and endpoint files, auth token, single instance,
auto-spawn from clients, foreground mode, idle shutdown (config), version
handshake, RPC wiring for `daemon.*`, `config.*`, `auth.*`, `session.*`,
`trace.*`, `stats.*` handlers (delegating to core), graceful in-flight
completion.
Out: OS service installation/autostart (later, optional), remote access.

## Design

### Process

```
harnessd [--foreground] [--stdio] [--home DIR] [--log-level L]
```

Startup sequence:
1. Resolve paths (M00-03); create data dir; init logging (M00-04).
2. Acquire `<data_dir>/daemon.lock` (advisory file lock via `fs4`; fail with
   exit code 3 "already running (pid N)" if held).
3. Open trace store (M00-06), load config, build `Mentor` (M00-05) lazily on
   first use (so a missing key does not prevent the daemon from starting).
4. Bind local socket (M00-02), generate token, write
   `<data_dir>/daemon.json` atomically:
   `{"pid", "endpoint", "token", "api_version", "version", "started_at"}` with
   0600 permissions.
5. Serve until shutdown signal.

Shutdown: on `daemon.shutdown`, SIGTERM/CTRL-C, or console close on Windows —
stop accepting connections, cancel running agents (they record
`agent.finished{cancelled}`), flush the trace writer queue, remove
`daemon.json`, release the lock. Hard deadline 10 s then exit.

Idle shutdown: if `daemon.idle_shutdown_min > 0` and no connections and no
running agents for that long, exit cleanly (default 0 = never, since the
inference service warm-up will be expensive in M02).

### Client discovery and spawn (`apprentice-client`)

```rust
DaemonClient::connect(ConnectOptions { home, spawn_if_missing: true, spawn_timeout: 10s })
```
1. Read `daemon.json`; if present, try connecting and `daemon.hello`. On
   success return.
2. If missing/stale (connect refused or pid not alive): if
   `spawn_if_missing`, locate the `harnessd` binary — next to the current
   executable, then `HARNESS_DAEMON_PATH`, then `PATH` — and spawn it detached
   (Windows: `CREATE_NO_WINDOW | DETACHED_PROCESS`; Unix: `setsid`, stdio to
   the log file). Poll `daemon.json` + connect every 100 ms up to the timeout.
3. `api_version` mismatch: if the daemon is older than the client and
   `spawn_if_missing`, ask it to shut down and spawn the current one; else
   error `incompatible_api` with both versions in the message.

The GUI (M00-10) bundles `harnessd` as a Tauri sidecar so the "next to the
executable" rule finds it.

### RPC handlers wired here

`daemon.hello/status/shutdown`, `config.get/set/path`, `auth.set_key/status`,
`session.create/list`, `trace.list/get`, `stats.tokens/reprice` — each a thin
adapter from API params to core calls with error mapping. `agent.run/cancel`
are wired in M00-11. M00-03/06/07 already ship `ConfigService`,
`TraceService` and `StatsService` with `register(router)`; `StatsService::new`
takes the `ConfigLoader` so `[pricing]` edits apply without a restart.

Concurrency: each connection is a task; handlers run on the tokio runtime;
the trace store is shared via `Arc`; core state in `AppState`
(`Arc<RwLock<Config>>`, `Arc<TraceStore>`, `OnceCell<Arc<dyn Mentor>>`,
`AgentRegistry` (M00-11)). `config.set` reloads config and rebuilds the
mentor on next use.

### CLI hooks (implemented in M00-09, defined here)

`harness daemon start` (spawn detached, print pid/endpoint), `harness daemon
stop` (shutdown RPC, wait for exit), `harness daemon status`, `harness daemon
run` (foreground, for development).

## Acceptance

- [x] Starting a second daemon exits with code 3 and a message naming the pid.
- [x] `daemon.json` is 0600 (Unix) / current-user-only ACL (Windows) and is
      removed on clean shutdown; a stale file with a dead pid is treated as
      absent.
- [x] Client with no daemon running spawns one and connects within 2 s on the
      reference machine; a second client reuses it.
- [ ] Graceful shutdown with an in-flight (mock) mentor call records
      `agent.finished{cancelled}` and flushes all queued events before exit.
      *Mechanism in place (shared `CancellationToken`, ordered drain, trace
      flush; in-flight RPC replies are delivered before close). The agent
      part lands with the runtime in M00-11, which also owns this test.*
- [x] Wrong token → `unauthorized`; older-daemon case triggers restart.
- [x] Windows: closing the console of a `--foreground` daemon triggers the
      graceful path (SetConsoleCtrlHandler via `ctrlc`/`tokio::signal`).

## Verification

Integration tests in `crates/daemon/tests/lifecycle.rs` running the real
binary with `HARNESS_HOME` temp dirs (use `assert_cmd`); the spawn test runs
on the current OS in CI.

## Notes

- Keep the daemon free of GUI assumptions; it must run headless on a server
  in M08.
- Detached spawning on Windows needs `CommandExt::creation_flags`; make sure
  the child does not inherit the parent's console or stdio handles.

## Completion notes (2026-09-12)

- **Process** (`crates/daemon`): `main.rs` (flags, paths, logging, lock,
  runtime, exit codes 0/1/3), `lock.rs` (`DaemonLock` on `daemon.lock` via
  `fs4`; the pid in the "already running" message comes from `daemon.json`),
  `lifecycle.rs` (`serve_socket` / `serve_stdio`, `daemon.status`,
  `daemon.shutdown`, signals, idle timer, ordered shutdown with a 10 s
  watchdog thread). Startup to `daemon.json` takes ~250 ms in a debug build.
- **Endpoint name** hashes `<user>@<data_dir>`, so several `HARNESS_HOME`s
  of one user (tests) get distinct pipes. Token: 256 random bits (two UUID
  v4s), never logged.
- **`daemon.json`** is written by `DaemonInfo::write` in `apprentice-client`
  (temp file + rename; `0600` on Unix; on Windows the ACL is replaced by a
  single full-control ACE for the current user through `icacls`, since the
  security API needs `unsafe`, which the workspace denies). `harnessd` now
  depends on `apprentice-client` for this type.
- **Core** (`apprentice_core::app::AppState`): loader, store, `TraceWriter`,
  secrets, lazily built mentor (`mentor()` / `invalidate_mentor()`), the
  shutdown `CancellationToken`, `session.create` (config snapshot per
  workspace, written through the writer), and `register(router)` which wires
  `config.*`, `auth.*`, `session.*`, `trace.*`, `stats.*`. `ConfigService`
  gained `with_on_change` so `config.set` / `auth.set_key` drop the built
  mentor. An invalid user config still lets the daemon start on defaults.
- **Router** (`apprentice-api`): `serve_with_shutdown` finishes in-flight
  handlers and writes their replies before closing (so a `daemon.shutdown`
  caller gets its answer); `daemon.shutdown` carrying the daemon `token` is
  accepted without a handshake so an *older* daemon (whose `daemon.hello`
  rejects us with `incompatible_api`) can still be asked to stop;
  `Router::with_api_version` exists for that compatibility test.
- **Client**: `DaemonClient::connect(&ConnectOptions)` → `Connected {client,
  hello, info, spawned}`. Stale = connect or handshake transport failure (no
  pid probing needed: a dead pipe/socket refuses at once). Binary lookup:
  explicit → next to the executable → `HARNESS_DAEMON_PATH` → `PATH`. Spawn
  is detached (Windows `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`, Unix
  `process_group(0)`; stdin/stdout null, stderr appended to
  `logs/daemon.stderr.log`), polled every 100 ms; a child exiting with 3
  means another instance won and polling continues. Dropping the last
  `DaemonClient` clone now closes the transport (the reader task holds a
  `Weak`), which is what lets a `--stdio` daemon and the idle timer notice.
- **Idle shutdown**: `daemon.idle_shutdown_min`, or the hidden
  `--idle-secs N` flag for tests; counts connections only until M00-11 adds
  running agents.
- **Tests**: `crates/daemon/tests/lifecycle.rs` runs the real binary: second
  instance exit 3 naming the pid; stale info ignored, spawn, shared reuse,
  `session.create/list`, `trace.list`, `daemon.status`, wrong token,
  `NotRunning`; older daemon replaced (real router pretending API v0);
  newer daemon → `Incompatible`; idle exit; `--stdio`; CTRL_BREAK on Windows
  (via `windows-sys` in a dev-dependency, same handler path as console
  close; `CloseMainWindow` cannot reach a Windows Terminal tab so that was
  not scripted); SIGTERM on Unix (written, not run here). The old
  `logging.rs` tests now run `--stdio` with a closed stdin so the daemon
  exits at once.
- The CLI's `with_client` now uses `DaemonClient::connect` with spawning
  off; M00-09 turns it on and adds `--no-spawn`, `harness daemon *`.
