# M01-15 — Outcome signals and dogfood readiness

Status: todo
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

- [ ] `run_tests` auto-detects the runner in fixture repos for cargo,
      pnpm, pytest and go; parsers produce correct pass/fail counts from
      recorded outputs (golden tests).
- [ ] A `shell` call `cargo test` yields a `tests` outcome without using
      `run_tests`.
- [ ] Accept/Reject/Done from GUI and CLI produce `outcome` events on the
      right agent.
- [ ] Stalled-agent watchdog fires in a test with a hanging mock.
- [ ] Two-week trial started with the checklist file in place (exit
      criterion of M01 is judged at the end of the trial).

## Verification

Golden tests for parsers; E2E for outcome emission; the trial itself.

## Notes

- Outcome labels are noisy by nature (tests may pass for reasons unrelated
  to the task); M04 combines them with judges. Record faithfully, do not
  interpret here.
