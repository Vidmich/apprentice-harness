# M02-08 — GUI model manager

Status: todo
Depends on: M01-12, M02-02, M02-03, M02-05, M02-07
Size: M

## Goal

A "Models" area in the GUI: the catalogue and installed models with
download progress and removal, per-role assignment with rollback, the
hardware panel (profile, planner table, VRAM bars), live service metrics
(loaded model, slots, queue, bypass rate, tokens/s) while the daemon
serves, warnings when a plan does not fit, and a card for the last bench
run with a button to start one. Everything is RPC from M02-02/03/04/05/07;
the GUI decides nothing.

## Context

SPEC §13 ("Model manager UI: download, hardware detection, per-role
assignment, adapter versions, rollback"; "apprentice contribution view"
is M03-10, not here). M01-12 established the Settings/sidebar
conventions (Zustand stores, `config.get/set` per key with sources, the
mock daemon under `pnpm dev`, `TESTING.md`).

## Scope

In: the Models view (sidebar entry `Ctrl+M`), catalogue/installed lists,
pull/remove flows, assignment UI, hardware and plan panel, live metrics
panel, bench card, warnings, the mock daemon's models/inference/bench
methods, Vitest for the pure parts, `TESTING.md` additions.
Out: adapter version browsing beyond what the manifest lists (M06 adds
the promotion history), the apprentice contribution view (M03-10), eval
dashboards (M04-10).

## Design

### Layout (`components/models/`)

```
ModelsView
  ├─ HardwarePanel      profile (GPU name, backend badge, VRAM free/total bar, RAM, CPU), "refresh"; the plan for the default model as a table (weights on GPU, KV per slot, slots × ctx, total, fits ✓/✗) with the planner's notes; a warning banner when fits_gpu is false or harnessd was built without the GPU's backend
  ├─ ServicePanel       inference.status polled every 2 s while visible: loaded model + adapter, slots busy/total, queue depth, resident states, bypass rate (last 5 min, from metrics), p50/p95 per role, prefill/decode tokens/s; "unload" / "load"; an "encoder pool" line (M02-06)
  ├─ InstalledList      models.list: name, kind badge (gguf/adapter/encoder), size, quant, context, state (ready/partial/corrupt), rated roles, "fits" from the plan; row actions: info (drawer with the manifest and its plan), remove (second click; warns about assignments), set as default
  ├─ CatalogList        models.catalog with "fits" column and licence; "Pull" → progress bar (models.progress events on the pull's subscription; resumable: a partial shows "resume"), cancel; "Add file…" (dialog → models.add) and "Add from repo…" (repo/file/revision fields)
  ├─ RolesPanel         one row per role (default, compressor, compactor, selector, observer; later gate): a select over compatible entries (gguf for generative roles, encoder for selector, adapter suffix when the base matches), the config source (user/workspace tab like Settings), "rollback" (models.rollback, enabled when a previous value exists), a "not rated for this role" hint with a force checkbox
  └─ BenchCard          last report from models.bench_list (pass/fail, per-role p95 and bypass, tokens/s, when), "Run bench" (agents, duration, --states) → progress bar from bench.progress, "open report" (the Markdown in a drawer), "suggest settings" → shows the [inference] block with an "apply" button (config.set of each key)
```

Settings gets a short "Inference" section (the `[inference]` keys:
slots, context, gpu_layers, kv_cache_type, resident_states, budgets per
role as a small table) with the same read/write mechanism as M01-12, and
a link to the Models view.

### Behaviour

- Pulls survive navigating away (the store keeps the subscription;
  a toast on completion); closing the app leaves the daemon's download
  running and the view re-attaches by `models.list` state `partial`
  plus the pull's running subscription id from `models.pulls`.
- Assignment changes call `models.assign` (not `config.set` directly)
  so validation and the rollback log are the daemon's; the roles panel
  reloads on `config.changed` (new event, emitted by the daemon on any
  `config.set` — also useful to M01-12's Settings, which currently
  re-reads on focus).
- Warnings: plan does not fit (`fits_gpu: false`) → banner naming the
  reduction the planner would make; backend not built → banner with the
  feature to build with; an assigned model missing or corrupt → banner
  with "re-pull"; bypass rate over 5 % in the last 5 minutes → a status
  bar dot next to the model name (the M03 apprentice view builds on it).
- Status bar (M00-10/M01-12) shows the loaded local model and a small
  activity indicator while slots are busy.

### Mock daemon (`src/lib/mock.ts`)

Models, catalogue, a pull that progresses over 5 s (resumable when
cancelled at 50 %), assignment with rollback history, a hardware profile
and planner table for a fake 12 GB GPU and a CPU-only variant
(`?hw=cpu`), `inference.status` with drifting metrics and a bypass burst
on demand, a bench that streams progress for 10 s and produces a report,
`config.changed`.

## Acceptance

- [ ] Against the mock: pull a catalogue entry → progress → installed;
      cancel at 50 % → partial → resume completes; remove asks twice
      and warns when assigned.
- [ ] Assign compressor to an installed gguf, an adapter with a matching
      base appears as `model@adapter`, an encoder is only offered for
      selector; rollback restores the previous assignment; the workspace
      tab writes the workspace layer (source shown).
- [ ] Hardware panel shows the profile and plan; switching the mock to
      `?hw=cpu` shows the CPU plan and the "no GPU backend" banner;
      a 7B entry shows the "reduced to N slots" note.
- [ ] Service panel updates every 2 s while shown and stops polling when
      hidden (network log); the bypass burst turns on the status-bar dot
      and it clears after 5 min of mock quiet.
- [ ] Bench card runs the mock bench with progress, shows pass/fail and
      the per-role table, "suggest settings" + "apply" writes the keys
      (visible in Settings → Inference).
- [ ] Real daemon on the reference machine (`TESTING.md` M02 section):
      pull the 3B candidate from the GUI, assign it as default, load,
      run `harness apprentice run` from the CLI and watch the slot go
      busy in the panel, run the bench from the card, read the report.
- [ ] Vitest: the pure helpers (`lib/models.ts`: fits/plan formatting,
      progress folding, role compatibility, warning derivation) and the
      `config.changed` handling; `pnpm typecheck` and `pnpm lint` clean;
      `api.ts` mirrors every new method and event with goldens read from
      the Rust snapshots.

## Verification

`pnpm test`, `pnpm typecheck`, `pnpm lint`; the mock walkthrough; the
real-daemon checklist on the reference machine.

## Notes

- Sizes in the UI are binary units (GiB) to match what `nvidia-smi` and
  the planner report; the CLI prints the same.
- Do not compute anything about memory in the GUI; the planner's table
  is the truth and the GUI renders it (the same rule as M00-10's "no
  business logic in the frontend").
