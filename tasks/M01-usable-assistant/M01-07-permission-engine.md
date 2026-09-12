# M01-07 — Permission engine

Status: todo
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

- [ ] Rule matching unit tests: precedence across layers, first-match
      within layer, glob and prefix and regex conditions, built-in deny-list.
- [ ] Ask flow over RPC: request event → respond → tool proceeds; deny →
      tool result `is_error` "denied by user"; timeout → deny.
- [ ] `allow_workspace` writes a rule to `.harness/permissions.toml` that
      matches the next identical call without asking.
- [ ] Headless deny/allow_readonly behaviours.
- [ ] `plan` mode: mentor's write attempt is denied and the reason is in the
      tool result.
- [ ] Every decision has a `permission.decision` event.

## Verification

`cargo test -p apprentice-core permissions::` plus an E2E with the CLI
prompt using `assert_cmd` with piped stdin.

## Notes

- The redacted `input_view` shown to the user must be exactly what will
  run (full command, full path) — never abbreviate in the prompt.
- Rules files are user-editable; parse errors must name file and line and
  fall back to `ask` for everything, not `allow`.
