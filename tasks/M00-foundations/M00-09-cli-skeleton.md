# M00-09 — CLI skeleton (`harness`)

Status: done
Depends on: M00-02, M00-08
Size: S

## Goal

The `harness` binary with the command tree, global flags, output conventions
(human vs `--json`), exit codes, and the M00 commands implemented against the
daemon: `daemon`, `config`, `auth`, `session`, `run`, `trace`, `stats`,
`doctor`. Later milestones add subcommands to this tree without changing
conventions.

## Context

SPEC §14 (CLI reaches parity with the GUI; indicative command set). The CLI
is a thin client: it must not link `apprentice-core`. All behaviour lives in
the daemon.

## Scope

In: clap structure, global flags, output formatting helpers, exit codes,
streaming display for `run`, shell completions, man-page-style `--help` text.
Out: interactive `chat` TUI (M08), tools/permissions commands (M01), models
(M02), bench/train (M04+).

## Design

### Global flags

```
harness [--home DIR] [--json] [--quiet] [--log-level L] [--no-spawn] [--timeout S] <command>
```

`--json`: every command prints exactly one JSON document (or NDJSON for
streaming commands) to stdout; human messages go to stderr. `--no-spawn`:
fail instead of starting a daemon.

### Command tree (M00)

```
harness daemon start|stop|status|run
harness config path | get [KEY] | set KEY VALUE [--workspace DIR]
harness auth set-key [--provider anthropic] [--stdin]      # prompts with hidden input unless --stdin
harness auth status
harness session new [--workspace DIR] [--title T] | list [--limit N]
harness run "<prompt>" [--session ID | --workspace DIR] [--model M] [--effort E] [--no-apprentice]
harness trace list [--session ID] [--agent ID] [--kind K]... [--limit N] | show EVENT_ID [--blob]
harness stats tokens [...]  (M00-07) | reprice [...]
harness doctor [--json]  (M00-04)
harness completions <shell>
```

`run` without `--session` creates a new session in the current directory as
workspace (or `--workspace`). It streams `agent.text_delta` to stdout as it
arrives, prints tool activity (M01) to stderr, and ends with a usage line on
stderr: `↳ in 1,204 · out 310 · cache read 0 · $0.0138 · 4.2s`. With `--json`
it prints NDJSON events followed by a final `{"type":"result", ...}` object.
Exit code 0 on `ok`, 2 on `error`, 130 on cancel (CTRL-C sends
`agent.cancel` then waits for `agent.finished`).

### Output helpers

`crates/cli/src/out.rs`: `print_table(rows)`, `print_kv(pairs)`,
`emit_json(value)`; colours via `owo-colors` only when stdout is a TTY and
`NO_COLOR` unset. Errors: `error: <message>` on stderr, `data.kind` shown in
brackets, e.g. `error: mentor rejected the request [mentor_error] (401 authentication_error)`.

### Exit codes

0 ok · 1 usage/argument error · 2 command failed · 3 daemon unavailable ·
4 unauthorized/incompatible · 130 interrupted.

## Acceptance

- [x] `harness --help` lists all commands with one-line descriptions;
      `harness <cmd> --help` documents flags and examples.
- [x] `harness auth set-key` stores via the daemon; `auth status` shows
      configured without revealing the key; the key never appears in
      `--json` output or logs.
- [x] `harness run "Say hi"` (against a mock or live daemon) streams text and
      prints the usage line; `--json` emits valid NDJSON ending in `result`.
      *Against a mock router; the daemon serves `agent.run` from M00-11.*
- [x] CTRL-C during `run` cancels cleanly with exit 130 and the trace shows
      `agent.finished{cancelled}`. *Exit 130 and `agent.cancel` are tested
      against the mock; the trace assertion is M00-11's cancellation E2E.*
- [x] `harness completions powershell|bash|zsh|fish` output loads without
      error.
- [x] `--no-spawn` with no daemon exits 3.

## Verification

`assert_cmd` tests in `crates/cli/tests/` against a daemon started with a
temp `HARNESS_HOME` and a mock mentor (`HARNESS_MENTOR_BASE_URL` pointing to
`wiremock`).

## Notes

- Keep command names stable; SPEC §14 names are the contract. Aliases are
  fine (`harness s` for `session`) but not required now.
- M00-08 shipped `DaemonClient::connect(&ConnectOptions)` (discovery, spawn,
  older-daemon replacement) and `crates/cli/src/daemon.rs::with_client`
  already uses it with `spawn_if_missing(false)`; `--no-spawn` maps onto
  that flag, `--home` onto `ConnectOptions::home` (only pass it when the
  user gave `--home`; platform paths must not be turned into `--home`).
  `ConnectError::NotRunning` / `Incompatible` are the exit-3 / exit-4 cases.
  `harness daemon start` = connect with spawning on, print pid/endpoint from
  `Connected`; `stop` = `daemon.shutdown` then poll `DaemonInfo::read` until
  absent; `run` = exec `harnessd --foreground` (locate via
  `apprentice_client::connect::locate_daemon`).

## Completion notes (2026-09-12)

- **Layout** (`crates/cli/src`): `main.rs` (clap tree, `Ctx`, exit-code
  mapping), `out.rs` (`Out`: `emit_json`/`emit_json_line`, `print_kv`,
  `print_table`, `info`, colours via `owo-colors` when stdout is a TTY and
  `NO_COLOR` is unset, `FORCE_COLOR` for tests; `describe` renders errors
  as `message [kind] (details)`), `daemon.rs` (`with_client`, `daemon
  start|stop|status|run`), `config.rs`, `auth.rs`, `session.rs`, `run.rs`,
  `trace.rs`, plus the earlier `stats.rs` and `doctor.rs`.
- **Global flags**: `--home`, `--json`, `--quiet`, `--log-level`,
  `--no-spawn`, `--timeout S` (per-request; 0 = forever). They parse before
  or after the subcommand. Log lines go to stderr only when a level was
  asked for (`--log-level` / `HARNESS_LOG_LEVEL`); the CLI log file gets
  them regardless.
- **Exit codes**: clap usage errors are remapped from clap's 2 to 1;
  `ConnectError::NotRunning|DaemonNotFound|Spawn|DaemonExited|SpawnTimeout`
  and `ClientError::Closed|Io` → 3; `Incompatible` and RPC
  `unauthorized`/`incompatible_api` → 4; `Exit::Interrupted` → 130;
  everything else 2. With `--json` a failure prints one `{"error": {...}}`
  document unless the command already printed its document (`run`'s
  `result` line carries the error).
- **`run`**: creates a session (cwd or `--workspace`) unless `--session`;
  `call_streaming::<AgentRun>`; text deltas to stdout, tool activity,
  `log` events and the usage line (`↳ in … · out … · cache read … · $… ·
  …s`) to stderr, `--show-thinking` for thinking deltas. Cost comes from
  the new optional `cost_usd` on the `agent.usage` event (additive;
  M00-11 fills it from the pricing table) and is omitted when absent.
  CTRL-C (and CTRL-BREAK on Windows) sends `agent.cancel` and keeps
  draining until `agent.finished`; a second press exits 130 at once.
- **`daemon start`** always spawns (ignores `--no-spawn`); **`stop`**
  (`--force` = non-graceful) and **`status`** never spawn; `stop` on no
  daemon is a no-op exit 0; **`run`** execs `harnessd --foreground` and
  waits for it through CTRL-C so the shell prompt returns after the
  daemon's shutdown.
- **`config set`** parses VALUE as JSON first (`true`, `8000`, `null`
  removes), string otherwise; `--workspace` writes the workspace layer.
  `config get` with no key prints the resolved tree as TOML.
- **`auth set-key`** prompts hidden (`rpassword`) on a terminal, reads the
  first stdin line with `--stdin` or when stdin is a pipe; then reports the
  store (`file`/`keychain`) from `auth.status`.
- **Bug found and fixed in `apprentice-client`**: on Windows a spawned
  daemon inherited the CLI's stdio pipe handles (`CreateProcess` inherits
  every inheritable handle), so `$(harness session new)` and the tests
  blocked until the daemon exited. `spawn_detached` now marks the three
  std handles non-inheritable (`SetHandleInformation`, the one `unsafe`
  call in non-test code, justified with `reason`).
- **Tests** (`crates/cli/tests`): `cli.rs` (help for every command, exit
  1 for usage, completions for five shells + loading the PowerShell script
  in `pwsh`, `--no-spawn`/stale/`status`/`stop` without a daemon, missing
  binary → 3), `run.rs` (fake router: streamed text and usage line,
  options pass-through, NDJSON with `result`, agent error → 2, CTRL-BREAK
  → `agent.cancel` → 130), `daemon.rs` (real `harnessd`, found next to
  `harness` or built with `cargo build -p harnessd`: spawn on first use,
  reuse, status, sessions, traces, config round trip, `auth` with the file
  store and no key in any output or log, wrong token → 4, stop, `daemon
  run` in the foreground).
