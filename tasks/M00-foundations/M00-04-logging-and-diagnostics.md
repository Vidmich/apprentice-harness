# M00-04 — Logging and diagnostics

Status: done
Depends on: M00-01, M00-03
Size: S

## Goal

Structured logging for daemon, CLI and GUI backend via `tracing`, written to
rotating files under `<data_dir>/logs/` and (for the daemon in foreground /
the CLI) to stderr; a `harness doctor` command that prints environment,
paths, versions, daemon state and detected hardware, for bug reports.

## Context

SPEC §3.1 (Config: logging), §15 (all data local). Diagnostics are needed
from the first day of dogfooding (M01) so failures during the two-week trial
can be reported precisely.

## Scope

In: tracing setup, file rotation, log levels from config/env/flags, request
ids in spans, redaction, `doctor` command, log location in RPC status.
Out: GUI log viewer (later), telemetry (never, by design).

## Design

- `apprentice_core::telemetry::init(role: "daemon"|"cli"|"gui", level, log_dir)`:
  `tracing_subscriber` with two layers — `fmt` (compact, ANSI when tty) to
  stderr and `tracing_appender::rolling::daily` JSON lines to
  `<log_dir>/<role>.log` (keep 14 files; prune on startup).
- Level precedence: `--log-level` flag > `HARNESS_LOG_LEVEL` > config
  `daemon.log_level` > `info`. `RUST_LOG` honoured for per-target filters.
- Spans: every RPC request gets `span!(rpc, method, id, conn)`; every mentor
  call gets `span!(mentor, call_id, model)`; every tool call (M01)
  `span!(tool, call_id, name)`. Agent and session ids are span fields so a
  grep on the log for an agent id shows its whole life.
- Redaction: a `tracing` field formatter that replaces values of fields named
  `api_key`, `token`, `authorization` with `***`. The `Secret` newtype from
  M00-03 already hides itself.
- Panics: `std::panic::set_hook` logs the panic with backtrace at `error`
  before the process exits; daemon converts panics inside request handlers
  into -32603 responses (`tokio::spawn` + `JoinError` check) so one bad
  request does not kill the daemon.
- `harness doctor` (CLI, no daemon needed) prints: harness/daemon/cli
  versions, OS, arch, config path, data dir and sizes, log dir, daemon
  status (running? pid? api version), auth status (configured, source),
  GPU/CPU summary (`wgpu` or `nvml` not required yet — for M00 read
  `nvidia-smi` if present, else "unknown"), free disk. `--json` for machine
  output.
- `daemon.status` result (M00-02) gains `log_file` path.

## Acceptance

- [x] Daemon writes JSON log lines to `logs/daemon.log`; rotation creates
      dated files; more than 14 old files are pruned.
- [x] `HARNESS_LOG_LEVEL=debug` changes verbosity without restarting the
      terminal session; `--log-level trace` overrides it.
- [x] A log line containing an API key value never appears (test injects a
      known fake key into a debug log call through the redaction path).
- [x] A handler panic returns -32603 and the daemon keeps serving.
- [x] `harness doctor` runs with and without a daemon, with `--json`.

## Verification

Unit tests for the redaction formatter and panic-to-error conversion; manual
run of `harness doctor` on Windows and inspection of the log dir.

## Notes

- Keep log volume sane: mentor request/response bodies are NOT logged (they
  are in the trace store); log only sizes, ids, usage and timings.

## Completion notes (2026-09-12)

- New crate `crates/common` (`apprentice-common`): `paths` (moved from
  `core::config`, re-exported there unchanged) and `telemetry`. The CLI and
  GUI need both and must not link `core`; `apprentice_core::telemetry` is a
  re-export so the name in this task still holds.
- `telemetry::init(&Options)`: `registry` + `EnvFilter` (level, then
  `RUST_LOG` directives appended) + JSON file layer via
  `tracing_appender::rolling` daily, `<log_dir>/<role>.<YYYY-MM-DD>.log`,
  `max_log_files(14)` prunes on start and at rotation + optional compact
  stderr layer. Returns `Handle { log_dir, log_file, filter }`; second call
  → `AlreadyInitialized`. `resolve_level(flag, env, config)` and
  `level_from_env()` implement the precedence.
- Redaction is a writer-level filter (`Redacting<MakeWriter>` →
  `RedactingWriter`), not a field formatter: the JSON formatter bypasses
  custom field formatters for event fields, so filtering the finished line
  is the only way to cover both layers. Field names `api_key`, `token`,
  `authorization` and `_`-suffixed variants (`daemon_token`) are blanked in
  JSON and compact forms; `input_tokens` etc. are untouched.
- Panic hook logs message, location, thread and backtrace at `error`
  (target `panic`) then chains to the previous hook. Handler panics were
  already isolated by the router (M00-02); the router now also wraps each
  handler in an `rpc` span (`method`, `id`, `conn`) and logs `elapsed_ms`.
- `harnessd`: clap flags `--foreground --stdio --home --log-level`; logging
  comes up even when the config is invalid (level falls back to
  flag/env/info and the config error is logged). Lifecycle still M00-08.
- `harness`: clap skeleton with the global flags of M00-09 (`--home --json
  --quiet --log-level`) and `doctor [--json]`. Doctor uses `sysinfo`
  (system + disk features) for OS/CPU/memory/free disk and pid liveness,
  `nvidia-smi` for GPUs (5 s timeout), reads `daemon.json`
  (`apprentice_client::DaemonInfo`, shape from M00-08) and, when the daemon
  answers, `daemon.status` + `auth.status`. Without a daemon only the
  `ANTHROPIC_API_KEY` environment source can be reported; keychain/file
  need the daemon. The CLI's own level is flag > env > info (it cannot read
  config without the core).
- Tests: 6 unit + 2 integration in `common` (JSON lines, redaction, pruning
  to 14, single init, panic hook), 2 daemon integration tests (flag > env >
  config; broken config still logs), 3 CLI integration tests (no daemon,
  stale/unreadable `daemon.json`, live router on a named pipe → `running`
  with auth via daemon). `harness doctor` run manually on Windows: RTX 3080
  Ti detected via nvidia-smi.
- Deferred: GUI logging init (M00-10 calls `telemetry::init("gui", ...)`),
  the `mentor`/`tool` spans (M00-05 / M01-01).
