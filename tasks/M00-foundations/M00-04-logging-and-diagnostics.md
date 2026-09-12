# M00-04 — Logging and diagnostics

Status: todo
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

- [ ] Daemon writes JSON log lines to `logs/daemon.log`; rotation creates
      dated files; more than 14 old files are pruned.
- [ ] `HARNESS_LOG_LEVEL=debug` changes verbosity without restarting the
      terminal session; `--log-level trace` overrides it.
- [ ] A log line containing an API key value never appears (test injects a
      known fake key into a debug log call through the redaction path).
- [ ] A handler panic returns -32603 and the daemon keeps serving.
- [ ] `harness doctor` runs with and without a daemon, with `--json`.

## Verification

Unit tests for the redaction formatter and panic-to-error conversion; manual
run of `harness doctor` on Windows and inspection of the log dir.

## Notes

- Keep log volume sane: mentor request/response bodies are NOT logged (they
  are in the trace store); log only sizes, ids, usage and timings.
