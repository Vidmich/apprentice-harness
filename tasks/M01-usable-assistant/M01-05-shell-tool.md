# M01-05 — Shell tool

Status: done
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

- [x] Runs `echo hi` on Windows and Unix with correct exit code, duration,
      streams separated and combined transcript stored. —
      `tools::shell::tests::echo_hi_reports_the_exit_code_the_duration_and_the_streams`,
      `stderr_is_tagged_and_a_nonzero_exit_is_not_an_error`; the trace
      side (transcript blob, `stdout`/`stderr` attachment blobs) in
      `tests/file_tools.rs::shell_stores_the_transcript_and_the_streams_apart`.
- [x] Timeout kills a sleeping process and its children (spawn a child that
      spawns a child; both gone after timeout). —
      `the_timeout_kills_the_command_and_its_children` (the grandchild
      writes its pid; `tasklist` / `kill -0` say it is gone).
- [x] Cancel from the agent kills the tree within 1 s. —
      `cancelling_the_agent_kills_the_tree_within_a_second`.
- [x] 20 MB output: blob stores head/tail with marker; mentor text obeys
      wrapper limits; GUI receives streamed progress without stalling the
      daemon (bounded channel, drops oldest with a marker if the consumer
      lags). — `big_output_is_capped_and_streamed_without_stalling`
      (a consumer 40 ms behind per chunk; the run is not slowed, the
      consumer gets `[... N lines of output not shown]`);
      `capture::tests` for the head/tail cut and the reporter.
- [x] Scrubbed env vars are absent inside the command (test prints env). —
      `scrubbed_variables_are_gone_and_the_hints_are_set`,
      `program::tests::environment_scrubs_and_adds_hints`.
- [x] Background job: start, poll output, kill. —
      `background_jobs_start_poll_wait_and_kill`,
      `a_background_job_dies_with_the_agent_unless_detached`.

## Verification

`cargo test -p apprentice-core tools::shell::` (platform-conditional cases)
and a manual run of a real test suite through the GUI.

## Notes

- Never run commands through `cmd.exe` on Windows: quoting is unreliable and
  the mentor writes POSIX-ish commands more consistently for PowerShell 7.
  The system prompt (M01-09) tells the mentor which shell it is talking to.

## Completion notes (2026-09-12)

Module `crates/core/src/tools/shell/` (`cargo test -p apprentice-core
tools::shell::`; platform-conditional only in the test commands, which
come in a PowerShell and a POSIX form):

- `mod.rs` — the `shell` and `shell_jobs` tools (`shell_tools()`, in
  `builtin_tools()`; they share one job registry per daemon). Input as
  designed plus `detach` (only with `background`). The mentor reads the
  combined transcript and a footer: `exit <code> · <duration>`,
  `killed by signal N · <duration>` (Unix), `timed out after N s;
  process tree killed` (an error result), `killed · <duration>` (a job
  killed through `shell_jobs`). Summary `exit 1 in 3.2 s`. Metadata
  `{exit_code, signal, duration_ms, timed_out, killed, stdout_bytes,
  stderr_bytes, stdout_lines, stderr_lines, truncated, orphans_killed,
  shell, description, progress_dropped_lines?}`.
- `program.rs` — shell selection: `tools.shell.program`/`args`, else
  `pwsh.exe` found on `PATH` (else `powershell.exe`) with `-NoProfile
  -NonInteractive -Command`, else `$SHELL -lc` (else `/bin/sh`). For
  PowerShell the command gets a trailer that makes the process exit
  with the command's own status (`-Command` alone maps every failure
  to 1): `$?` of the last statement, `$LASTEXITCODE` of a failed native
  command, `exit N` as written. Environment: the daemon's minus
  `tools.shell.scrub_env` (`*` patterns, case-insensitive; default
  `ANTHROPIC_API_KEY`, `*_API_KEY`, `*_TOKEN`, `*_SECRET`,
  `*_SECRET_*`, `*_PASSWORD`, `AWS_*`) plus `HARNESS=1`, `NO_COLOR=1`,
  `TERM=dumb` and `tools.shell.env` (for `CI=1` and the like; empty by
  default).
- `process.rs` — spawn with stdin null, pipes for both streams,
  `CREATE_NO_WINDOW`; a Job Object with `KILL_ON_JOB_CLOSE` (Windows;
  `windows-sys`, the module's only `unsafe`) or a process group of its
  own (Unix; `libc::killpg`). `Tree::kill` is idempotent and `Drop`
  kills too, so a call the executor abandons on cancellation still
  takes its tree down. The pump reads both pipes concurrently (64 KiB,
  cut at line ends), ends when the child exited *and* the pipes closed;
  something still holding them 500 ms after the exit is killed
  (`orphans_killed`), and a kill that leaves pipes open is abandoned
  after 2 s.
- `capture.rs` — `Capture` (head ¾ / tail ¼ of the budget, marker
  `[... N bytes omitted]`, cuts on line ends when one is within 4 KiB
  and on UTF-8 boundaries otherwise), `Transcript` (combined + one
  capture per stream; a change of stream is a `[err]` / `[out]` line,
  a leading stdout run has none, so stdout-only output reads as itself
  and deduplicates with its attachment blob), `Reporter` (progress
  through `ctx.progress` with `try_send`; up to 8 chunks queue for a
  lagging consumer, then the oldest are dropped and the next delivery
  is preceded by `[... N lines of output not shown]`).
- `jobs.rs` — `Jobs` registry: ids `j1`, `j2`, ...; a task per job
  pumps into an in-memory `Transcript` under the same caps; a
  `watch` channel for `wait`; the last 50 finished jobs stay listed.
  Killing goes through the job's cancel token — the agent's own child
  token unless `detach: true`, which gets a fresh one (and, on Windows,
  a job object without `KILL_ON_JOB_CLOSE`).

Tool-system changes: `ToolContext.config: Arc<ToolsConfig>` (the
executor clones the session's tools config once per step);
`ToolOutput.attachments` — named side outputs the executor stores as
blobs of their own and lists in the `tool.result` payload as
`attachments: [{name, blob_id, bytes, media_type}]` (`shell` attaches
`stdout` and `stderr` when non-empty; they are capped by the tool).
Config: `tools.shell.{program, args, scrub_env, env, max_timeout_s}`
(workspace-overridable like the rest of `tools.`).

Output caps: the combined transcript and each stream keep at most
`tools.max_capture_bytes` (8 MiB) each, head and tail, so a 20 MB run
costs about 24 MiB of memory at the worst and the blob never needs a
second cut; the mentor's copy is then cut by the wrapper to
`max_mentor_bytes`. The `shell` spec sets the wrapper timeout to
3660 s so the tool's own `timeout_s` (≤ 3600, further capped by
`tools.shell.max_timeout_s`) always wins and the partial output is
returned; `shell_jobs` (read-only; wrapper timeout 660 s for `wait`,
≤ 600 s) only observes or stops what the agent itself started.

Deviations / decisions:

- `shell_jobs` has a fourth action, `wait` (up to `timeout_s`, default
  30 s), so the mentor need not poll a long build.
- `shell_jobs` is `Risk::ReadOnly`: the job was permitted when it was
  started; the permission engine (M01-07) still sees `detach: true` on
  the `shell` call that asks for it.
- Output bytes are kept as they came (CRLF from PowerShell cmdlets,
  `\r` progress lines); tests normalise. Invalid UTF-8 is shown as
  U+FFFD in the mentor's text only.
- Jobs are killed when the agent's cancellation token fires (agent stop,
  daemon shutdown); the runtime of M01-08 decides whether a normal end
  of the agent cancels it too. Detached jobs survive the daemon on both
  platforms.
- On Unix a finished foreground call kills its process group on the
  way out (as `KILL_ON_JOB_CLOSE` does on Windows), so a command that
  leaves a daemon behind must use `background: true`.
- The GUI round trip waits for M01-08/M01-11 (no consumer of
  `ToolProgress` yet); verified with the unit and executor tests and
  `harness tools list --describe`.
