# M01-15 — Outcome signals and dogfood readiness

Status: done
Depends on: M01-06, M01-08, M01-11, M01-12
Size: S

## Goal

Every trajectory ends with machine-readable outcome signals — test results
detected from tool runs, files changed, explicit user acceptance/rejection,
task-done marks — so that later milestones can label baseline trajectories
as successful or not without re-reading transcripts. Plus the small set of
robustness items needed before the two-week daily-use trial.

## Context

SPEC §9 (task outcome signals), §10 (outcome signals feed training), M01
exit criterion (used for real work for two weeks). Success labels are what
"remote tokens per *successful* task" divides by.

## Scope

In: `outcome` event producers, `run_tests` convenience tool, user
accept/reject/done controls (GUI + CLI), crash recovery checks, first-run
experience, dogfooding checklist.
Out: feedback ratings and notes (M05 — but the `outcome` events here are
distinct from feedback), judge-based success (M04).

## Design

### Outcome kinds (payload `outcome {kind, details}`)

| kind | producer | details |
|---|---|---|
| `files_changed` | runtime from snapshots (M01-06) | changed/added/deleted lists |
| `tests` | `run_tests` tool and heuristics on `shell` outputs | `{runner, passed, failed, skipped, exit_code, duration_ms, command}` |
| `build` | heuristics on `shell` outputs (`cargo build`, `tsc`, `pnpm build`, `pytest --collect-only`…) | `{ok, exit_code, command}` |
| `user_accept` / `user_reject` | GUI buttons / CLI `harness session mark` | `{note?}` |
| `task_done` | user marks the session's task complete | `{}` |
| `error` | runtime | `{kind}` (refusal, max_iterations, context_limit, mentor_error) |
| `reverted` | snapshot diff at next agent start shows the previous agent's changes undone (best effort) | `{files}` |

### `run_tests` tool — Risk::Execute

Input `{command?: string, cwd?}`; if no command, detect the runner from
the workspace (`Cargo.toml` → `cargo test`, `package.json` scripts.test →
`pnpm test`, `pyproject.toml` → `uv run pytest`, `go.mod` → `go test
./...`) and tell the mentor which one was used. Runs via the shell tool
machinery (same permissions) and parses the output with per-runner
parsers (cargo test summary line, pytest summary, jest/vitest summary, go
test) into the `tests` outcome; unknown runner → exit-code-only outcome.
The mentor receives the normal shell output plus a one-line parsed
summary at the top.

Heuristic detection for plain `shell` calls: if the command matches a
known test/build invocation, run the same parser on its output and emit
the outcome — so the mentor using `shell` directly still yields labels.

### User controls

GUI: at the end of each turn, small buttons "✓ Accept" / "✗ Reject" and
in the session header "Mark task done"; CLI: `harness session mark ID
accept|reject|done [--note]`. Each emits an `outcome` event attached to
the last agent.

### Robustness before dogfooding

- Daemon crash → GUI reconnects and the open session resumes (M01-10
  invariants); a crash report line in the log with the last RPC method.
- First run: if no API key, Setup screen (M00-10); if no workspace,
  prompt to add one; sample `.harness/HARNESS.md` template offered.
- Unhandled mentor error kinds surface as readable system notes, never
  silent stalls; a watchdog marks an agent `error: stalled` if no event
  for `mentor.timeout_s + 60s`.

### Dogfooding checklist (`docs/dogfood-m01.md`)

Daily: use the harness for real work; note friction in the file; check
`harness stats tokens` and `harness trace replay-check --since 1d`. Weekly:
export a redacted bundle to a safe location; count trajectories with a
`tests` or `user_accept` outcome (target ≥ 60% labelled).

## Acceptance

- [x] `run_tests` auto-detects the runner in fixture repos for cargo,
      pnpm, pytest and go; parsers produce correct pass/fail counts from
      recorded outputs (golden tests). — `outcomes::runners::tests::
      workspace_manifests_pick_the_runner` (Cargo.toml → `cargo test`,
      package.json with a `test` script → `pnpm`/`npm`/`yarn test` by
      lockfile, pyproject.toml → `pytest` / `uv run pytest`, go.mod →
      `go test ./...`, in that precedence); `tests/runners.rs` reads
      thirteen recorded outputs under `tests/fixtures/runners/` (cargo
      ok / failed / compile error, nextest, pytest ok / failed / no
      tests, vitest ok / failed, jest, go with and without `-v`) into
      the `runner_outputs` golden and asserts the counts;
      `tests/outcomes.rs::run_tests_and_a_shell_cargo_test_both_yield_a_tests_outcome`
      runs `run_tests {}` on a real crate in the workspace: `cargo test`
      detected from Cargo.toml, `2 passed, 1 skipped`, the parsed line on
      top of the mentor's transcript, the outcome at the step.
- [x] A `shell` call `cargo test` yields a `tests` outcome without using
      `run_tests`. — the same test's second run (`shell {command:
      "cargo test"}`): `tests` outcome with `source: shell`, the same
      counts; `outcomes::runners::tests::commands_are_classified_by_their_first_word_and_subcommand`
      covers the command heuristics.
- [x] Accept/Reject/Done from GUI and CLI produce `outcome` events on the
      right agent. — `marks_land_on_the_right_agent_and_count_as_labels`
      (over the router: `session.mark` defaults to the last run, takes
      `agent_id`, refuses a session without runs and a foreign agent;
      `session.get` lists the marks per run; `stats.outcomes` counts
      them); GUI: `transcript.test.ts` "keeps outcomes per run"
      (`agent.outcome` folding, `addOutcome` from the RPC answer,
      `verdictOf`); CLI parse test `outcome_commands_parse`.
- [x] Stalled-agent watchdog fires in a test with a hanging mock. —
      `the_watchdog_ends_a_silent_run_as_stalled`: a tool that never
      returns and ignores cancellation, every wait set to 2 s and
      `stall_grace_s = 1` → the run ends after 3 s as `error` with kind
      `stalled`, `outcome {error, stalled}`, the tool call `cancelled`
      in the trace.
- [x] Two-week trial started with the checklist file in place (exit
      criterion of M01 is judged at the end of the trial). —
      `docs/dogfood-m01.md` (before / daily / weekly / end-of-trial
      lists and a notes table); the trial itself starts with the first
      day of real use after this commit and is judged at its end.

## Verification

Golden tests for parsers; E2E for outcome emission; the trial itself.

## Notes

- Outcome labels are noisy by nature (tests may pass for reasons unrelated
  to the task); M04 combines them with judges. Record faithfully, do not
  interpret here.

## Completion notes (2026-09-13)

`cargo test -p apprentice-core` (`tests/outcomes.rs` new: six
integration tests over `AppState`; `tests/runners.rs` new: the parser
goldens; unit tests in `outcomes::`, `outcomes::runners::`,
`workspace::snapshot`, `permissions::tests`), `-p apprentice-api --test
snapshots` (`session_get` grew `outcomes`/`error`, `session_mark`,
`stats_outcomes`, `workspace_init`, `events` has `agent.outcome`), `-p
harness` (parse, `format_outcome_stats`, `outcomes_line`); `pnpm test`
(41; the shapes read from the goldens, the transcript folding).

- Outcomes (`crates/core/src/outcomes/`): `Outcome {kind, details}` is
  recorded as `outcome {kind, details}` — the payload shape M01-06/08
  already used — and `describe(kind, details)` derives the one-line
  summary and the verdict (`ok`) clients show, so events written before
  this task get one too. Kinds: `files_changed` (as before, now also a
  live event), `tests`, `build`, `user_accept`, `user_reject`,
  `task_done`, `error` (`refusal`, `max_iterations`, `context_limit`,
  `stalled`, `daemon_restart`), `reverted`. `kind::LABELS` (`tests` and
  the marks) is what "labelled" means in `stats.outcomes`.
- Runners (`outcomes/runners.rs`): `detect_runner(dir)` from the
  manifests; `classify_command` reads a command line (chains split on
  `&&`, `;`, `||`, newlines; env assignments skipped; the family's
  known word found wherever it is, so `cargo +nightly test`, `pnpm
  --dir apps/gui test`, `uv run --directory ml pytest` work) into
  `Tests(runner?)` — cargo/nextest, pytest (also `python -m pytest`,
  `uv|poetry|pdm run …`), pnpm/npm/yarn/bun test and run scripts,
  vitest/jest, `go test`, `just|make test` (no parser) — or `Build`
  (`cargo build|check|clippy`, `tsc`, `pnpm build|typecheck`, `go
  build|vet`, `pytest --collect-only`, `just check`). Parsers: libtest
  summaries summed over binaries, nextest's `Summary` line, pytest's
  closing `=== … in Ns ===` (errors → failed, xfailed → skipped,
  xpassed → passed), vitest's `Tests  N passed | M failed` / jest's
  `Tests: …` (last line wins), go's `--- PASS/FAIL/SKIP` else the
  package lines (`unit: packages`). `recognised: false` when no summary
  is found: the outcome is then exit-code-only (`parsed: false`), never
  a false zero.
- `run_tests` (`tools/shell/run_tests.rs`, `Risk::Execute`, tags
  `shell`, `tests`, default timeout 600 s): `{command?, runner?, cwd?,
  timeout_s?, description?}`; without `command` the runner is detected
  in `cwd` (an error result names the four manifests when none is
  found); spawn/pump/render are the shell tool's; the mentor gets
  `[tests] 2 passed, 1 skipped (cargo test)` (or `[tests] no summary
  recognised; tests: exit 101 (…)`) above the transcript; the metadata
  carries `command`, `runner`, `detected_from` and the outcome details.
  The runtime (`runtime/agent.rs::run_tools`) records the outcome from
  the metadata; for a foreground `shell` call it classifies the command
  and parses the mentor-facing text (`Executed` now carries the tool's
  `metadata`). Recorded at the step, emitted as `agent.outcome`.
- Marks: `session.mark {id, mark: accept|reject|done, note?,
  agent_id?}` → `{agent_id, outcome}`; the event goes on the named run
  or the session's last one (`not_found` without a run,
  `invalid_params` for an agent of another session); a running agent's
  subscribers get `agent.outcome`. CLI `harness session mark ID
  accept|reject|done [--note TEXT] [--agent ID]`; `session show` prints
  a `run <id>: …` line per run with outcomes. GUI: **✓ Accept** / **✗
  Reject** in the turn footer once the run ended (the verdict replaces
  them), **Mark task done** in the session header (`✓ done` after), the
  outcomes line under each footer (green/red by verdict), live from
  `agent.outcome` and from the stored rows (`AgentSummary.outcomes`,
  merged by event id).
- Watchdog (`Activity` on `AgentHandle`, touched by every emitted
  event): a task per run checks every `window/4` (0.2–15 s) and, once
  nothing was emitted for [`stall_window`] = max(`mentor.timeout_s`,
  `tools.timeout_s.execute`, `permissions.ask_timeout_s`) +
  `runtime.stall_grace_s` (60; `0` disables), marks the run stalled and
  cancels it; the loop's cancelled finish becomes `error` kind
  `stalled` with `outcome {error, stalled, window_s}`. With the defaults
  that is 660 s — the task's `mentor.timeout_s + 60s` — and the other
  two waits are in the max so a long permission prompt or a silent
  execute tool within its own timeout is not mistaken for a stall.
- Daemon restart: `TraceStore::recover_orphaned_agents` at
  `AppState::open_with` ends every agent still `running` (steps and
  mentor calls too) as `error` with `agent.finished {error: {kind:
  daemon_restart}}` and `outcome {error, daemon_restart}`, logged with
  the ids. `AgentSummary.error` (the stored `agent.finished` error) now
  travels with `session.get`, so the GUI shows how a run ended after a
  reload; on reconnect `recoverAfterReconnect` reloads every chat that
  was following a run (reattached when the new daemon has it, else
  closed with the stored error in its footer). Crash report: the panic
  hook takes a context closure (`install_panic_hook_with`); the daemon
  passes the last RPC method the router dispatched
  (`apprentice_api::server::last_method`), logged as `crash report:
  panic at … (last rpc method: …)` before the backtrace line.
- `reverted` (`runtime/revert.rs`): at a run's start snapshot, the
  previous run's `files_changed` is checked — an added file that is
  gone, a deleted one that is back, and (same `HEAD`, git available) a
  changed file that the previous end snapshot's diff named and the new
  start's does not. The outcome goes on the previous run
  (`at_agent` names the one that noticed) and is emitted on the new
  run's subscription with the previous agent's id.
- `stats.outcomes {since?, until?, workspace_id?}` → `OutcomeStats
  {agents, labelled, labelled_share, by_kind, tests_passed,
  tests_failed, accepted, rejected, done, errors}` over the main agents
  started in the range; `harness stats outcomes` prints the share
  against the 60 % target. `trace replay-check` gained `--since` /
  `--until` (the checklist's `--since 1d`).
- First run: `workspace.init {id, force?}` writes the `HARNESS.md`
  template (`conflict` when present); `harness workspace init [DIR|ID]
  [--force]`; the GUI's workspace panel offers "Add the template" when
  `has_instructions` is false. The Setup screen (no key) and the
  workspace picker (no workspace) were already there.
- Permissions: the built-in catastrophic denies now match any tool that
  carries a `command` (`run_tests {command: "rm -rf /"}` is denied like
  `shell`); a rule for `tool = "run_tests"` allows it without a prompt.
- `docs/dogfood-m01.md`: the checklist (before / daily / weekly / end)
  and the notes table; README points at it.

Deviations / decisions:

- `build` outcomes come from the `shell` heuristics only; `run_tests`
  is the one convenience tool (no `build` tool).
- The heuristics read the mentor-facing result text, not the raw blob:
  the summaries the parsers look for are at the end of the output,
  which the middle-cut truncation keeps.
- `reverted` for modified files needs git (the end snapshot's diff);
  without it only added/deleted files are judged. A commit between the
  runs means "kept", not "reverted".
- The watchdog counts the tool timeouts and the permission prompt in
  its window (see above) rather than the mentor timeout alone: with the
  defaults the number is the task's, and the false positives are gone.
- `stats.outcomes` counts main agents by their start time; `labelled`
  is the union over the label kinds, `tests_passed`/`tests_failed` use
  a run's last `tests` outcome.
- Marks attach to the last agent by default rather than refusing while
  one runs: accepting the previous turn while the next one streams is
  the natural moment.
