# M01-05 — Shell tool

Status: todo
Depends on: M01-01, M01-02
Size: M

## Goal

A `shell` tool that runs a command in the workspace with a timeout,
streams output to the GUI/CLI while running, captures stdout/stderr and
exit code fully into the trace, and is safe by default (Risk::Execute, so
always subject to the permission engine).

## Context

SPEC §3.1 (shell command, run tests). Shell output (build logs, test runs)
is the single biggest token sink in coding agents and the primary target of
the compressor role; capture it completely and with structure (exit code,
duration, stream separation).

## Scope

In: command execution, shell selection per OS, cwd, env scrubbing,
timeouts, cancellation (kill process tree), streaming progress, output
capture and limits, background mode for long-running processes.
Out: interactive PTY sessions, sudo/elevation, remote execution.

## Design

Tool `shell` — Risk::Execute. Input:

```json
{"command": "string", "cwd": "relative dir (optional)", "timeout_s": 120, "background": false, "description": "one line for the user"}
```

Execution:
- Windows: `pwsh -NoProfile -NonInteractive -Command <cmd>` if `pwsh` is
  found, else `powershell`; Unix: `$SHELL -lc <cmd>` falling back to
  `/bin/sh -c`. Configurable: `tools.shell.program`/`args`.
- Env: inherit the daemon's env minus a scrub list (`ANTHROPIC_API_KEY`,
  `*_TOKEN`, `*_SECRET`, `AWS_*`, configurable) plus `HARNESS=1`,
  `NO_COLOR=1`, `CI=1`-style hints off by default, `TERM=dumb`.
- Process group / job object: Unix `setsid` + kill the group on
  cancel/timeout; Windows: assign to a Job Object with
  `KILL_ON_JOB_CLOSE` so child trees die too.
- Streaming: stdout/stderr read concurrently, line-buffered; each chunk
  sent on `ctx.progress` (`ToolProgress::Output{stream, text}`); the runtime
  emits `agent.tool_progress` (M01-08) for the GUI console.
- Capture: both streams fully (up to 8 MiB each, then head/tail with a
  marker), plus interleaving order preserved in a combined transcript with
  `[out]`/`[err]` tags for the trace blob. Result text for the mentor:
  combined transcript truncated by the wrapper policy (M01-01), then a
  footer `exit <code> · <duration>` — or `timed out after Ns` /
  `cancelled`.
- Exit code ≠ 0 → `is_error: false` (the mentor needs to read failures as
  normal output; the footer conveys the code) but `metadata.exit_code` set
  and `summary` says `exit 1 in 3.2 s`.
- `background: true`: start the process, return immediately with a
  `job_id`; `shell_jobs` tool (`list|output|kill`) manages them; output is
  spooled to a blob file as it arrives. Jobs die with the agent unless
  `detach: true` (asks permission separately).

Permissions (M01-07) see the full command string and `description`; rules
can allow by prefix (`cargo test*`, `git status`), which is how "allow in
workspace" stays useful.

## Acceptance

- [ ] Runs `echo hi` on Windows and Unix with correct exit code, duration,
      streams separated and combined transcript stored.
- [ ] Timeout kills a sleeping process and its children (spawn a child that
      spawns a child; both gone after timeout).
- [ ] Cancel from the agent kills the tree within 1 s.
- [ ] 20 MB output: blob stores head/tail with marker; mentor text obeys
      wrapper limits; GUI receives streamed progress without stalling the
      daemon (bounded channel, drops oldest with a marker if the consumer
      lags).
- [ ] Scrubbed env vars are absent inside the command (test prints env).
- [ ] Background job: start, poll output, kill.

## Verification

`cargo test -p apprentice-core tools::shell::` (platform-conditional cases)
and a manual run of a real test suite through the GUI.

## Notes

- Never run commands through `cmd.exe` on Windows: quoting is unreliable and
  the mentor writes POSIX-ish commands more consistently for PowerShell 7.
  The system prompt (M01-09) tells the mentor which shell it is talking to.
