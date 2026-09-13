# M01-07 — Permission engine

Status: done
Depends on: M00-02, M01-01, M01-02
Size: M

## Goal

Every tool call passes through a permission decision: allowed by rule,
denied by rule, or asked of the user through whichever client is attached
(GUI dialog or CLI prompt). Rules are per workspace and per user, can be
created from a prompt answer ("always allow in this workspace"), and every
decision is a trace event.

## Context

SPEC §3.1 Permission engine (modes: ask / allow in workspace / allow always /
deny; prompt in GUI and CLI). Also the safety net for the Executor role in
M07, which must obey the same engine.

## Scope

In: rule model and files, matching, decision flow with client prompting
over RPC, timeouts, headless behaviour, CLI/GUI-facing API, trace events,
`harness tools allow|deny|rules` commands.
Out: GUI dialog visuals (M01-12), sandboxing at OS level.

## Design

### Rule model

```toml
# ~/.config/apprentice-harness/permissions.toml   (user layer)
# <workspace>/.harness/permissions.toml            (workspace layer)
default = "ask"                      # ask | deny  (never "allow" as default)

[[rule]]
tool = "read_file"                   # exact tool name or "*"
effect = "allow"                     # allow | deny | ask
[rule.match]                         # all listed conditions must hold
path = "src/**"                      # glob on resolved root-relative path (file tools)
command_prefix = "cargo test"        # for shell: command starts with this (after trim)
outside_workspace = false            # true to match only out-of-root paths

[[rule]]
tool = "shell"
effect = "deny"
[rule.match]
command_regex = "\\brm\\s+-rf\\b|Remove-Item.*-Recurse"
```

Built-in defaults (lowest precedence): all `ReadOnly` tools inside the
workspace → allow; `Write` inside workspace → ask; `Execute`/`Network` →
ask; anything `outside_workspace` → ask; a deny-list of catastrophic shell
patterns → deny (user can override explicitly in their own file).

Precedence: workspace deny > user deny > workspace allow > user allow >
built-ins > default. Within a layer, first matching rule wins (file order).

### Decision flow

```rust
pub enum Decision { Allow, Deny{reason}, Ask }
pub async fn decide(&self, req: &PermissionRequest, prompter: &dyn Prompter) -> Result<Outcome>
```

`PermissionRequest { tool, risk, input (redacted view), workspace_id,
paths: Vec<PathBuf>, command: Option<String>, description }`.

When `Ask`: the engine emits `permission.request {request_id, agent_id,
tool, input_view, risk, suggested_rules: [...]}` to the agent's subscriber
and awaits `permission.respond {request_id, answer}` where `answer ∈
{allow_once, allow_session, allow_workspace, allow_always, deny_once,
deny_always}`. `allow_session` is held in memory for the agent's session;
`allow_workspace`/`allow_always`/`deny_always` append a rule to the
corresponding file (generated from `suggested_rules`, e.g. `command_prefix`
= first two tokens of the command, or `path` = the directory glob).

Timeout: if no client responds within `permissions.ask_timeout_s` (default
600) → deny with reason `timeout`. No client attached (headless) →
behaviour from `permissions.headless = "deny" | "allow_readonly"`.

Each outcome → `permission.decision {request_id, tool, decision, source:
rule|user|timeout|headless, rule_ref?}` event.

### Runtime mode

`agent.run` options gain `permission_mode: "default" | "plan" | "auto"`:
`plan` forces deny on all Write/Execute (mentor is told it is read-only);
`auto` treats `Ask` as `Allow` for Write inside the workspace but still asks
for Execute/Network/outside — a dogfooding convenience, off by default.

### CLI

```
harness tools rules [--workspace DIR]            # effective rule list with sources
harness tools allow <tool> [--path GLOB | --command-prefix P] [--workspace|--user]
harness tools deny  <tool> [...]
harness run ... --permission-mode plan|auto
```

CLI prompt for `Ask`: `? shell: "cargo test -p core" (execute) [a]llow once / [s]ession / [w]orkspace / [A]lways / [d]eny / [D]eny always`.

## Acceptance

- [x] Rule matching unit tests: precedence across layers, first-match
      within layer, glob and prefix and regex conditions, built-in deny-list.
      — `permissions::tests::{conditions_glob_prefix_regex_risk_and_outside,
      precedence_across_layers_and_first_match_within,
      the_builtin_deny_list_catches_catastrophes_and_a_file_can_override,
      parse_errors_name_the_line_and_break_the_layer_into_ask,
      appended_rules_keep_the_file_sound_and_report_their_line}`.
- [x] Ask flow over RPC: request event → respond → tool proceeds; deny →
      tool result `is_error` "denied by user"; timeout → deny. —
      `tests/permissions.rs::a_prompt_answered_over_rpc_lets_the_tool_run`
      (`permission.respond` over the router), `the_broker_settles_a_prompt_
      with_the_first_answer`, `cancelling_the_agent_ends_a_pending_prompt`;
      the CLI prompt end to end in `cli/tests/run.rs::permission_prompts_
      are_answered_from_stdin` (piped stdin, `--json` denies).
- [x] `allow_workspace` writes a rule to `.harness/permissions.toml` that
      matches the next identical call without asking. —
      `answers_decide_and_write_rules`.
- [x] Headless deny/allow_readonly behaviours. — `timeout_headless_and_modes`.
- [x] `plan` mode: mentor's write attempt is denied and the reason is in the
      tool result. — `timeout_headless_and_modes` (the reason: "denied: plan
      mode is read-only and write_file is a write tool"),
      `the_gate_records_every_decision_and_the_executor_obeys` (the reason
      is the `tool_result` text).
- [x] Every decision has a `permission.decision` event. —
      `the_gate_records_every_decision_and_the_executor_obeys`,
      `tests/permissions.rs`.

## Verification

`cargo test -p apprentice-core permissions::` plus an E2E with the CLI
prompt using `assert_cmd` with piped stdin.

## Notes

- The redacted `input_view` shown to the user must be exactly what will
  run (full command, full path) — never abbreviate in the prompt.
- Rules files are user-editable; parse errors must name file and line and
  fall back to `ask` for everything, not `allow`.

## Completion notes (2026-09-12)

Module `core::permissions` (`cargo test -p apprentice-core permissions`,
`--test permissions` for the RPC round trip, `cargo test -p harness --test
run permission` for the CLI prompt):

- `rules.rs` — the file model (`default`, `[[rule]]` with `tool`,
  `effect`, `[rule.match]`: `path`, `command_prefix`, `command_regex`,
  `outside_workspace`, and `risk`, which the design did not have but
  which lets the built-ins be ordinary rules), parsed with `toml::Spanned`
  so every error names file and line (`permissions.toml:5: ...`), including
  a bad glob or regex. `Layer` loads one file and re-reads it when its
  mtime or size changes, so hand edits apply to the next call. `evaluate`
  ranks: workspace deny, user deny, a broken file (→ ask, with the error in
  the decision's `notes`), the first match of the workspace file, of the
  user file, the built-ins, the explicit `default` of the first file that
  sets one, else ask. Built-ins: `read_only_inside` (allow, `risk =
  read_only`, `outside_workspace = false`) and five `shell` denies
  (`rm_recursive_root`, `remove_item_recursive_root`, `disk_format`,
  `fork_bomb`, `chmod_recursive_root`); an explicit allow in a file wins
  over them. `append_rule` adds a rule with a comment through `toml_edit`
  (comments and layout of the file survive), re-parses before writing and
  refuses to append to a broken file.
- `mod.rs` — `PermissionRequest::for_call(spec, input, workspace)`: the
  paths from `path`, `cwd` and `paths` resolved through the workspace
  (root-relative `shown`, `outside` when the sandbox refuses them; a tool
  whose schema has `path`/`cwd` and an input without it names the root),
  `command`, and a one-line `description` (the shell's own, else
  `tool: command` / `tool: paths`). `input_view` cuts strings over 2 KiB
  except paths, patterns and the command, which the prompt shows whole.
  `suggest` builds the rules an answer writes: `command_prefix` of the
  first two words (then the first alone), or for a command chain a
  `command_regex` of the exact command, since a prefix never matches a
  chain; for paths the directory glob (`src/**`, `**` at the root) then
  the exact path, with `outside_workspace = true` for outside paths.
  `Engine::decide(req, agent, mode, prompter)`: plan → deny writes and
  commands; the files; the session's `allow_session` rules (in memory,
  per session, held by the `PermissionBroker`); auto → allow writes
  inside; then the prompt, timeout (`permissions.ask_timeout_s`, 600) and
  headless (`permissions.headless`: `deny` | `allow_readonly`).
- `broker.rs` — `PermissionBroker` (the pending prompts of the process,
  `permission.respond` settles one; first answer wins, the rest are
  `not_found`) and `AgentPrompter`, which emits `permission.request` on an
  `EventSink` (the agent's broadcast channel) and treats "nobody
  listening" as headless.
- `gate.rs` — `PermissionGate` implements `tools::Gate`: builds the
  request, decides, records `permission.decision {call_id, tool, risk,
  mode, decision, source, rule_ref?, reason?, request_id?, answer?, asked,
  waited_ms, paths, outside_workspace, command?, rule_written?, notes?}`,
  emits the live `permission.decision` event, and denies with the reason
  as the `tool_result` text. `Executor::run` now races the gate against
  the cancellation token, so cancelling an agent ends a pending prompt.
- RPC: `permission.respond {request_id, answer, rule?}`, `tools.rules
  {workspace?} → {rules: [RuleInfo{source, index, line?, name?, rule}],
  files: [RuleFileInfo]}`, `tools.allow` / `tools.deny {tool, match,
  layer, workspace?} → {path, line, rule}`. Events: `permission.request`
  gained `description`, `command`, `paths`, `suggested_rules`,
  `timeout_s`; `permission.decision` is new. `agent.run` options gained
  `permission_mode` (`default` | `plan` | `auto`). Config:
  `permissions.default_mode` is now that mode (the old value `"ask"` still
  loads as `default`), plus `ask_timeout_s` and `headless`.
- CLI: `harness tools rules [--workspace DIR | --user]`, `harness tools
  allow|deny <tool> [--path GLOB] [--command-prefix P] [--command-regex R]
  [--outside | --inside] [--risk R] [--workspace DIR | --user]`, `harness
  run --permission-mode plan|auto`. The prompt (stderr) shows the
  description, the risk, the rule `w`/`A` would write and the timeout,
  and reads one line from stdin (`a s w A d D`; empty or EOF denies once;
  anything else is asked again); a decision event for the pending request
  (the GUI answered) closes it. With `--json` a request is denied at once
  and said so on stderr. Denials by rule are printed as `✗ tool: reason`.
  `api.ts` carries the new types and events (the GUI dialog is M01-12).

Deviations / decisions:

- `command_prefix` never matches a command chain (`;`, `&`, `|`, a
  newline, a backtick, `$(`): otherwise `allow cargo test` would pass
  `cargo test; rm -rf .`. Chains need a `command_regex` rule, which is
  what the prompt suggests for them.
- A `path` glob matches an outside path only in a rule with
  `outside_workspace = true`; `path = "**"` alone stays inside the root.
- `default` in a file counts only when set explicitly, so a workspace file
  without one does not hide the user's `default = "deny"`.
- Session allow rules are consulted only where the files say ask: a deny
  rule holds against an earlier `allow_session`.
- `allow_workspace` in a session without a workspace allows once and says
  so in `notes`; a rule that cannot be written is a note too, never a
  changed decision.
- The runtime wiring (building the `Engine` per session and a
  `PermissionGate` per step with the agent's handle as the sink) is
  M01-08's, alongside the tool loop it gates; `tests/permissions.rs` runs
  that assembly by hand.
