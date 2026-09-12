# M00-09 — CLI skeleton (`harness`)

Status: todo
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

- [ ] `harness --help` lists all commands with one-line descriptions;
      `harness <cmd> --help` documents flags and examples.
- [ ] `harness auth set-key` stores via the daemon; `auth status` shows
      configured without revealing the key; the key never appears in
      `--json` output or logs.
- [ ] `harness run "Say hi"` (against a mock or live daemon) streams text and
      prints the usage line; `--json` emits valid NDJSON ending in `result`.
- [ ] CTRL-C during `run` cancels cleanly with exit 130 and the trace shows
      `agent.finished{cancelled}`.
- [ ] `harness completions powershell|bash|zsh|fish` output loads without
      error.
- [ ] `--no-spawn` with no daemon exits 3.

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
