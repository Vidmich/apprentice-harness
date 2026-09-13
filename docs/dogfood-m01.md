# Dogfooding M01 — the two-week trial

The exit criterion of M01 (`ROADMAP.md`): the harness is used for real
work for at least two weeks, the trace export produces a corpus, and a
replay of any recorded step reproduces the exact mentor request. This
file is the checklist for those two weeks and the place to note friction
as it happens. Start date: ______ · end date: ______.

## Before the first day

- [ ] `harness doctor` is clean; `harness daemon status` shows the
      daemon; the GUI connects.
- [ ] An API key is set (Setup screen, or `harness auth set-key`).
- [ ] Every project you will work in is a workspace (`harness workspace
      add DIR`, or "Add folder…" in the GUI), and each has a
      `.harness/HARNESS.md` worth reading (`harness workspace init DIR`
      writes the template; the GUI offers it when the file is missing).
- [ ] Permission rules for the commands you run all day (`harness tools
      allow shell --command-prefix "cargo test"` and the like), so runs
      do not stall on prompts you would always answer the same way.
- [ ] `harness config get runtime.stall_grace_s` is not `0`: a run that
      shows no sign of life is ended as `error: stalled` instead of
      hanging.

## Daily

- Use the harness for real work — the tasks you would otherwise give the
  Claude desktop app. Prefer the GUI; use `harness run` when a terminal
  is where you are.
- Let runs end with a label: `run_tests` (or a plain `cargo test` /
  `pytest` / `pnpm test` / `go test` through `shell`) yields a `tests`
  outcome by itself; click **✓ Accept** / **✗ Reject** under a turn and
  **Mark task done** in the session header, or `harness session mark ID
  accept|reject|done [--note ...]`. Unlabelled runs are what the weekly
  count is about.
- Note friction below the moment you feel it — a prompt you had to
  answer twice, a tool that returned too much, an answer that stalled,
  a wrong file touched — with the session id (`harness session list`).
- End of day:
  - [ ] `harness stats tokens --since 1d` — the day's spend; anything
        surprising goes in the notes.
  - [ ] `harness trace replay-check --since 1d` — every request body of
        the day passes; add `--rebuild` once a week (slower).
  - [ ] The daemon log has no `crash report:` line
        (`harness doctor` names the log file).

## Weekly

- [ ] `harness stats outcomes --since 7d` — the labelled share (runs
      with a `tests`, `user_accept`, `user_reject` or `task_done`
      outcome) is **≥ 60 %**. Below that, mark more runs; the labels
      are what "remote tokens per successful task" divides by.
- [ ] `harness trace export --since 7d -o <safe location>/harness-week-N.tar.zst`
      — a redacted bundle off the machine. Open it once: `uv run
      apprentice-ml traces stats <bundle>` (from `ml/`) and a look at
      `manifest.json`'s `redaction` block. Nothing that should be
      private is in it.
- [ ] `harness trace replay-check --since 7d --rebuild` passes.
- [ ] Read the notes below; anything that made you reach for another
      tool becomes a task (or a fix now).

## End of the trial

- [ ] Fourteen days with real use on most of them.
- [ ] The weekly bundles exist and import into a scratch home
      (`harness --home <scratch> trace import <bundle>`; `replay-check`
      passes there).
- [ ] `harness stats outcomes --since 14d` shows the labelled share.
- [ ] The notes are triaged into tasks.

## Notes

Date · session · what happened · what it cost you.

| date | session | note |
|---|---|---|
|      |         |      |
