# M00-08 — Daemon lifecycle and client discovery

Status: todo
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

- [ ] Starting a second daemon exits with code 3 and a message naming the pid.
- [ ] `daemon.json` is 0600 (Unix) / current-user-only ACL (Windows) and is
      removed on clean shutdown; a stale file with a dead pid is treated as
      absent.
- [ ] Client with no daemon running spawns one and connects within 2 s on the
      reference machine; a second client reuses it.
- [ ] Graceful shutdown with an in-flight (mock) mentor call records
      `agent.finished{cancelled}` and flushes all queued events before exit.
- [ ] Wrong token → `unauthorized`; older-daemon case triggers restart.
- [ ] Windows: closing the console of a `--foreground` daemon triggers the
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
