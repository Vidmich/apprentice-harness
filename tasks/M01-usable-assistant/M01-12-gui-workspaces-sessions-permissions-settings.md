# M01-12 — GUI: workspaces, sessions sidebar, permission dialog, settings

Status: done
Depends on: M00-10, M01-02, M01-07, M01-10
Size: M

## Goal

The surrounding app UI: pick/add workspaces, browse and search sessions,
start new ones, resume, rename, archive; answer permission requests in a
dialog with "always allow in workspace" options and a rules editor;
settings for API key, model, effort, thinking display, permission mode and
shell; multiple sessions running concurrently with status indicators.

## Context

SPEC §13 (workspaces, session list, resume, search, permission prompts,
per-workspace rules editor, multiple concurrent agents, settings). All
behaviour is RPC from M01-02/M01-07/M01-10/M00-03.

## Scope

In: sidebar (workspaces + sessions), new session flow, permission dialog
and rules editor, settings screen, concurrent-run indicators, tray/close
behaviour.
Out: model manager UI (M02), token panel (M01-13), apprentice views (M03).

## Design

### Sidebar

- Workspace switcher at top: list from `workspace.list`; "Add folder…"
  uses `tauri-plugin-dialog` → `workspace.add`; shows git branch and
  dirty marker from `workspace.info`.
- Sessions list for the selected workspace (`session.list`), grouped
  Today / Yesterday / Earlier; search box → `session.search`; each row:
  title, relative time, status dot (running = pulsing), cost badge;
  context menu: rename, archive, delete, export.
- "New session" button; `Ctrl+N`. Sessions of other workspaces reachable
  via "All workspaces" filter.

### Permission dialog

Triggered by `permission.request` events on any active subscription
(also when the session is not in the foreground: show a badge on the
session row and a non-modal toast; the dialog opens when the session is
focused). Content: tool name and risk badge, the exact command or path,
the mentor's `description`, and buttons `Allow once`, `Allow for session`,
`Allow in workspace`, `Always allow`, `Deny`, `Deny always` → respond via
`permission.respond`. Shows the rule that would be created (from
`suggested_rules`) with an editable glob/prefix before saving. Countdown of
the ask timeout.

Rules editor (Settings → Permissions → per workspace/user): table from
`tools.rules` with source column; add/remove rules → `tools.allow/deny`
RPC; raw file "open in editor" link.

### Settings

Sections: *Mentor* (API key set/replace via `auth.set_key`, status only;
model text field with the default suggested; effort select; thinking
display; max tokens), *Permissions* (default mode, headless behaviour,
rules), *Shell* (program/args), *Sessions* (auto-title on/off), *Data*
(paths from `config.path`, open folder, size; "Export traces" link to
M01-14), *About* (versions from `daemon.status`, log file link). Writes
via `config.set` to the user layer, with a per-workspace tab for the
overridable keys.

### Concurrency and window behaviour

- Multiple sessions may run at once; the header shows "N running"; each
  transcript has its own run state (M01-11 store keyed by session).
- Closing the window keeps the daemon alive; a tray icon (optional,
  `tauri-plugin-tray`) offers "Quit (stop daemon)". Quitting the app does
  not stop the daemon unless chosen.

## Acceptance

- [x] Add a folder, start a session, run, see it in the list with a running
      indicator, switch to another session and run there concurrently;
      both complete correctly. — `WorkspaceSwitcher` ("Add folder…" →
      `workspace.add`), `Ctrl+N` / "+ New session", the row's pulsing dot
      (`running_agent` from `session.list`, an API addition, or the local
      transcript), the status bar's `N running`; each chat keeps its own
      transcript and listener (M01-11). Checked against the mock: two
      runs at once, `2 running`, both transcripts complete.
- [x] Permission dialog appears for a write; "Allow in workspace" creates a
      rule visible in the rules editor; next identical call does not ask.
      — `permission.request` events of every listener are routed by
      session (`lib/permissions.ts`, `permissions.test.ts`); the dialog
      answers with `permission.respond` and the edited rule; the rules
      editor (`tools.rules`) shows it as `workspace:1`. Checked with the
      mock's `ask` prompt: the second run did not ask. The daemon half
      (the rule written, the engine honouring it) is `core/tests/permissions.rs`
      and the checklist.
- [x] Settings changes persist (visible via `harness config get`) and
      apply to new sessions. — `config.set` per key to the user or
      workspace layer, sources shown, `null` to reset; the daemon
      rebuilds its mentor on change and a session snapshots the config at
      creation (M01-10). The mock keeps its config in `sessionStorage`;
      the CLI check is in `TESTING.md`.
- [x] Search finds sessions by content; rename/archive/delete work. —
      `session.search` behind the box (debounced, matches highlighted),
      `session.rename` in place, `session.archive` (+ "show archived"),
      `session.delete` with a second click, `session.export` to a file.
      Checked against the mock.
- [x] Closing the window leaves the daemon running; reopening restores the
      last workspace and session. — the daemon is spawned detached
      (M00-10); the selected workspace joins the chats in `localStorage`
      (checked: a reload came back on the same workspace and session);
      the tray offers "Quit and stop the daemon" for the other case.

## Verification

Manual checklist in `apps/gui/TESTING.md`; unit tests for the
permission-event routing to the right session.

## Notes

- Never show or store the API key; the field is write-only.

## Completion notes (2026-09-13)

`pnpm test` (32 vitest tests: `permissions.test.ts`, `format.test.ts` new),
`pnpm typecheck`, `pnpm lint`; `cargo test -p apprentice-core` (permissions,
sessions, workspace, snapshots), `-p apprentice-api --test snapshots`,
`-p apprentice-harness-gui`.

- Sidebar: `WorkspaceSwitcher` (a select over `workspace.list`, "All
  workspaces", "Add folder…" through the dialog plugin, `×` forgets;
  the selected one's root, `⎇ branch` and a dirty `●` from
  `workspace.info`), "+ New session" / `Ctrl+N`, `⚙` / `Ctrl+,`, the
  search box, "show archived", the list grouped Today / Yesterday /
  Earlier with the drafts (chats without a session yet) on top. A row:
  title, status dot (pulsing while running, red after an error), a
  count badge while a permission waits, the cost, the relative time,
  and a menu (right-click or `…`): Rename (in place), Archive /
  Unarchive, Export… (save dialog + `write_text_file`), Delete (second
  click confirms). The list is re-read every 30 s while the window is
  shown, after every run, and after every mutation; deleted rows
  (`include_archived` lists them too) are left out.
- Permissions: `lib/permissions.ts` is the pure router (requests keyed
  by session; a decision naming the request, the agent's end, or the
  daemon's timeout closes it), `stores/permissions.ts` holds it,
  `chat.ts` feeds every listener's `permission.*` events in. The dialog
  shows tool + risk, description, command or paths, the input, the
  suggested rules (a select when several, the fields editable, "reset"),
  the six answers with the CLI's letters as keys, and the countdown
  from `timeout_s`. "Allow in workspace" is disabled without a
  workspace. Requests of sessions not in front: a badge on the row and
  a toast with "Show". `Esc` inside the dialog does not reach the chat's
  cancel.
- Rules editor (Settings → Permissions): `tools.rules` as a table
  (source:index, effect, tool, match, the file line), the files with
  their state and "open in editor" (`open_path`), an add form
  (`RuleFields`, `tools.allow`/`tools.deny` to the user or workspace
  file) and a remove per file rule — `tools.remove`, a new method
  (`ToolsRemoveParams {layer, workspace?, index}`), with
  `harness tools remove <INDEX> [--user | --workspace DIR]` for parity.
- Settings: sections Mentor (API key write-only via `auth.set_key`,
  status from `auth.status`; model, effort, thinking display, max
  tokens), Permissions (default mode, headless, ask timeout, rules),
  Shell (program, args one per line, max timeout), Sessions
  (auto-title), Data (`config.path`, "open" / "open folder", the data
  dir's size from `dir_size`, the M01-14 pointer), About (app and
  daemon versions, pid, uptime, open sessions, log files, restart, quit
  and stop). Every field is read with `config.get {key, workspace?}`
  for its source and written with `config.set` on change (blur/Enter
  for text); "reset" writes `null`. The Workspace tab (when one is
  selected) enables only the overridable keys.
- Concurrency: unchanged from M01-11 (one transcript and listener per
  session); the status bar counts sessions with a run in flight — the
  daemon's `running_agent` (new on `SessionSummary`, from the
  runtime's registry) or a busy transcript here. Up to 8 session chats
  stay open; opening more closes the oldest idle ones.
- Window and tray (`src-tauri/src/host.rs`): closing the window quits
  the app and leaves the daemon (spawned detached) running; the tray
  icon (`tauri` feature `tray-icon`) offers "Show", "Quit (daemon keeps
  running)" and "Quit and stop the daemon" (`daemon.shutdown` then
  exit); Settings → About has the same "quit and stop". New commands:
  `open_path`, `write_text_file`, `dir_size`, `quit_and_stop_daemon`;
  the capability adds `dialog:allow-save`.
- API additions (Rust + `api.ts`, snapshots updated): `SessionSummary.
  running_agent`, `WorkspaceInfoResult.git_dirty` (`git status
  --porcelain=v2` through the workspace's `Repo`; `None` without git or
  a work tree; `harness workspace info` prints `, dirty` / `, clean`),
  `tools.remove`.
- Mock (`src/lib/mock.ts`): workspaces, the list and search, the
  lifecycle methods, `config.*` with a default table and both layers,
  `tools.*` with rule files kept in `sessionStorage`, `permission.
  respond`, the host commands; the `ask` prompt word sends the edit
  through a small rule matcher and the ask flow.

Deviations / decisions:

- The chats of M01-11 stay as the "open" set behind the sessions list
  rather than a separate tabs UI: the list is the navigation, a draft
  shows as a "New session" row until its first send.
- Rules are removed by index (`tools.remove`) rather than edited in
  place: an edit is a remove and an add, and the file keeps its
  comments either way.
- The dirty marker counts untracked files (a fresh file is a change the
  user would want to know about), unlike `Status::is_dirty` in the
  snapshot, which counts tracked changes only.
- The window does not hide to the tray on close (an app that keeps
  running invisibly surprises more than it helps); the tray is there for
  the "stop the daemon" case and to bring a minimised window back.
- Session export goes through a save dialog and a backend write rather
  than the fs plugin (one command, no scope configuration).
- `session.list` with `include_archived` also returns deleted rows; the
  sidebar filters them out client-side rather than adding a parameter.
- The acceptance boxes were checked against the mock daemon; the
  real-app run (rules on disk, `harness config get`, the tray, a run
  from the CLI showing up) is in `TESTING.md` for the dogfood.
