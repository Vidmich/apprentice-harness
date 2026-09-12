# M01-12 — GUI: workspaces, sessions sidebar, permission dialog, settings

Status: todo
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

- [ ] Add a folder, start a session, run, see it in the list with a running
      indicator, switch to another session and run there concurrently;
      both complete correctly.
- [ ] Permission dialog appears for a write; "Allow in workspace" creates a
      rule visible in the rules editor; next identical call does not ask.
- [ ] Settings changes persist (visible via `harness config get`) and
      apply to new sessions.
- [ ] Search finds sessions by content; rename/archive/delete work.
- [ ] Closing the window leaves the daemon running; reopening restores the
      last workspace and session.

## Verification

Manual checklist in `apps/gui/TESTING.md`; unit tests for the
permission-event routing to the right session.

## Notes

- Never show or store the API key; the field is write-only.
