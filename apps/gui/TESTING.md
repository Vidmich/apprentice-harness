# GUI dogfooding checklist

Manual checks for the chat view (task M01-11). Run them in the app
(`just gui`) against a scratch repository with a real API key; the mock
daemon (`pnpm dev` in a browser, see the README) covers the rendering
half without spending tokens.

## Setup

- [ ] `just gui` starts the app; the status bar shows the daemon version
      and `key: configured`.
- [ ] A new chat's composer is disabled with "Choose a workspace folder to
      start" until a folder is picked (Browse… or typed).
- [ ] The first prompt creates the session: the header shows its title
      (the first line of the prompt), workspace and id; the sidebar row
      follows. After the first answer the cheap model's title replaces it
      within a few seconds.

## Streaming and rendering

- [ ] Assistant text streams without flicker; markdown renders as it
      arrives (headings, lists, tables, bold); a fenced code block is
      highlighted once its grammar has loaded and stays highlighted while
      it grows.
- [ ] Every code block has a language label and a working `copy` button;
      an answer has a `copy` button on hover.
- [ ] Links open in the OS browser, not in the window.
- [ ] Thinking shows as a collapsed "Thinking… (n chars)" line that
      expands; with `mentor.thinking_display = "omitted"` in the config
      (restart the daemon) no thinking line appears at all.
- [ ] The view sticks to the bottom while streaming; scrolling up stops
      that; scrolling back to the bottom resumes it.

## Tool cards (one per M01 tool)

Ask for something that reads, searches, edits, writes and runs:
"add a `hello` module with a unit test and run the tests".

- [ ] `read_file`, `glob`, `grep`, `list_files`: header shows the name, the
      summary, `ok`, the duration; _Input_ is the pretty JSON; _Result_ is
      the text the mentor got.
- [ ] `edit_file` / `write_file`: the _Diff_ tab opens by default with the
      hunk, line numbers and colours; "side by side" pairs the lines.
- [ ] `shell`: the card opens itself while output streams; _Console_
      autoscrolls, unticking "follow" stops it; the header ends with
      `exit 0 in …`.
- [ ] A result the daemon cut for the mentor shows the
      `[... N bytes omitted, full result id …]` marker highlighted in
      _Result_; _Raw_ has the whole output.
- [ ] _Raw_ with a 5 MB output (`shell` with `seq 1 400000`, or `cat` of a
      large file): loads from the trace, scrolls smoothly, the UI stays
      responsive; `copy all` works.
- [ ] A denied call (answer "deny" on the CLI prompt, or a `deny` rule)
      shows `denied` and the source.

## Turn footer, errors, retry

- [ ] Each turn ends with model · in/out/cache tokens · cost · duration ·
      time, the status (`ok`, `cancelled`, `error`), any warning
      (`context_large`, `tools_changed`) and "waiting … for the mentor"
      lines.
- [ ] Unplug the network (or store an invalid key with the CLI's
      `auth set-key`) and send: the turn shows the error with its kind
      and a `Retry` button; Retry re-sends the prompt. A run that already
      ran a tool has no Retry.
- [ ] Stop the daemon (`harness daemon stop`) during a run: the turn ends
      with `daemon_unavailable`; the app reconnects; the next send works.

## Cancel

- [ ] `Esc` (or the Cancel button) while text streams: the turn ends
      `cancelled` within a second; the partial text stays until the next
      send.
- [ ] Cancel while a `shell` runs (`sleep 60`): the card ends `error`
      with `shell: cancelled`; the turn is `cancelled`.

## Reload and reattach

- [ ] Reload the window (F5 in a debug build, or close and reopen the
      app) while a run streams: the chat comes back, the transcript shows
      the stored rows, the footer says `● running`, new text and tool
      cards keep arriving, and after the run ends the transcript is
      complete (no missing or duplicated text; the partial text from
      before the reload is replaced by the stored message).
- [ ] Reload after the run ended: the transcript is identical to what was
      shown live; expanding a card's _Raw_ tab still finds the output.
- [ ] A session with more than 100 messages: only the newest page loads;
      scrolling to the top loads the older page without the view jumping.
- [ ] A transcript with more than 200 items (a long session) scrolls
      smoothly (virtualised); streaming at the end still sticks.

## Composer and keyboard

- [ ] Enter sends, Shift+Enter breaks the line; the toggle under the box
      switches to Ctrl+Enter and survives a reload.
- [ ] Pasting a long text (≥ 12 lines or ≥ 1500 chars) becomes a chip with
      its size; it is sent inside `<pasted_text>` after the typed text;
      the × removes it.
- [ ] `Ctrl+L` focuses the composer from anywhere; `Esc` cancels a run.
- [ ] Send is disabled while a run is in flight; a second chat can run at
      the same time on another session.

## Accessibility and themes

- [ ] Tab order: chats, composer, cards' headers, their tabs. Cards
      announce as "<tool> tool call, <status>"; tabs are a `tablist`.
- [ ] Switch the OS theme between light and dark: colours, code
      highlighting and diff colours follow without a restart.

## Workspaces, sessions, permissions, settings (task M01-12)

Two checkouts help (any two folders; a git work tree shows the branch).

- [ ] Workspace switcher: "Add folder…" registers a folder and selects
      it; the row below shows its root, `⎇ branch` and a `●` when the
      tree has uncommitted changes (`↻` re-reads); `×` forgets it (its
      sessions stay, reachable under "All workspaces"); the CLI's
      `harness workspace list` agrees.
- [ ] "+ New session" (or `Ctrl+N`) opens a draft on the selected
      workspace; the first send creates the session and the row appears
      under _Today_ with a pulsing dot while it runs, then the cost
      badge and the relative time. A folder typed or browsed in the
      draft's header is registered too.
- [ ] Two sessions run at once: start one, switch to another (or
      `Ctrl+N`) and send there; the status bar says `2 running`, both
      rows pulse, both transcripts complete. A run started with
      `harness run` on the same daemon shows in the list within 30 s;
      clicking its row reattaches.
- [ ] Search: typing in the box shows message hits (newest first) with
      the matching words highlighted; a hit opens its session; the
      selected workspace filters the hits; `Esc` clears the box.
- [ ] Row menu (right-click or `…`): _Rename_ edits in place (Enter
      saves, Esc cancels) and the title survives a reload and the
      generator; _Archive_ hides the row (visible again with "show
      archived", _Unarchive_ brings it back); _Delete_ asks for a second
      click; _Export…_ saves the `session.export` JSON where you choose.
      Archive/Delete on a running session report `conflict`.
- [ ] Permission dialog: with no rule for it, a `write_file` outside the
      built-ins' scope (or a `shell` command) opens the dialog over the
      chat: tool and risk badge, the command or paths, the mentor's
      description, the countdown (`permissions.ask_timeout_s`), the
      suggested rule with editable fields. `Allow once` runs the call;
      the CLI's letters (`a s w A d D`) work as keys; `Esc` does not
      cancel the run while the dialog is up.
- [ ] `Allow in workspace` writes the rule (edit the glob first, e.g.
      `docs/**`): it shows in Settings → Permissions → Rules as
      `workspace:N` and in `harness tools rules`; the next identical
      call does not ask. `Deny always` writes to the user file; the
      card shows `denied`.
- [ ] A request on a session that is not in front: the row gets a
      count badge and a toast appears bottom-right; _Show_ brings the
      session up with the dialog. Answering from the CLI (a second
      `harness run --session` client) closes the dialog here.
- [ ] Settings (`⚙` or `Ctrl+,`): each field shows where its value
      comes from (`default` / `user` / `workspace` / `env`); changing
      one writes it (`harness config get <key>` agrees) and _reset_
      takes it out again; the _Workspace_ tab edits only overridable
      keys and marks the others. Model/effort/thinking display apply to
      the next session (`omitted` hides thinking after a daemon
      restart). The API key field is write-only and saving it updates
      the status bar.
- [ ] Rules editor: _Add rule_ appends to the chosen file; _remove_
      takes a file rule out; built-ins have no remove; "open in editor"
      opens the file. A broken file shows NOT IN FORCE with the line.
- [ ] Data: the paths match `harness config path`; "open folder" opens
      the data directory; the size is plausible. About: daemon version,
      pid, uptime, the log files open; "restart daemon" reconnects.
- [ ] Closing the window leaves `harnessd` running (`harness daemon
status`); reopening the app restores the selected workspace and
      the last session. The tray icon offers "Show", "Quit (daemon keeps
      running)" and "Quit and stop the daemon" — the last one stops it.
