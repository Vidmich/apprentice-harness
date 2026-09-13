//! The permission engine (task M01-07): every tool call is allowed by
//! rule, denied by rule, or asked of whichever client is attached.
//!
//! A call becomes a [`PermissionRequest`] (tool, risk, the paths it
//! names resolved against the workspace, the command it would run).
//! [`Engine::decide`] runs it through the run's mode (`plan` denies
//! writes and commands, `auto` allows writes inside the workspace), the
//! rule layers of [`rules`] (workspace file, user file, built-ins, the
//! file's `default`), the answers remembered for the session, and
//! finally a [`Prompter`] — the RPC-backed one in [`broker`] emits a
//! `permission.request` event and waits for `permission.respond`; with
//! no client attached `permissions.headless` decides, and nobody
//! answering within `permissions.ask_timeout_s` is a denial. An answer
//! of `allow_workspace`, `allow_always` or `deny_always` appends a
//! rule to the corresponding file so the next such call is not asked.
//!
//! [`PermissionGate`] is the [`crate::tools::Gate`] the executor calls;
//! it records every decision as a `permission.decision` trace event and
//! emits the live one.

pub mod broker;
mod gate;
pub(crate) mod rpc;
pub mod rules;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use apprentice_api::events::Risk;
use apprentice_api::types::{
    PermissionAnswer, PermissionDecision, PermissionMode, PermissionSource, RuleEffect, RuleMatch,
    RuleSource, RuleSpec,
};
use async_trait::async_trait;
use serde_json::Value;
use tracing::{debug, warn};

pub use broker::{AgentPrompter, EventSink, PermissionBroker};
pub use gate::PermissionGate;
pub use rpc::PermissionService;
pub use rules::{CompiledRule, Layer, RuleFileError};

use crate::config::{Headless, Paths, PermissionsConfig};
use crate::tools::ToolSpec;
use crate::trace::AgentId;
use crate::workspace::{PathError, Workspace};

/// The user's rules file, next to `config.toml`.
pub const USER_RULES_FILE: &str = "permissions.toml";

/// Strings in the prompt's `input` view longer than this are cut,
/// except paths and commands, which are shown whole.
pub const INPUT_VIEW_MAX_CHARS: usize = 2048;

/// Input keys shown whole in the prompt and used as the call's subject.
const PATH_KEYS: &[&str] = &["path", "cwd"];
const WHOLE_KEYS: &[&str] = &["path", "cwd", "paths", "command", "pattern", "glob"];

/// The user's rules file for `paths`.
pub fn user_rules_file(paths: &Paths) -> PathBuf {
    paths.config_dir.join(USER_RULES_FILE)
}

/// A path a call names, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectPath {
    /// As the mentor gave it.
    pub given: String,
    /// Root-relative with `/` (`.` for the root); absolute, `/`, when
    /// outside.
    pub shown: String,
    pub abs: Option<PathBuf>,
    /// Not under the workspace root (or there is no workspace, or the
    /// path could not be resolved).
    pub outside: bool,
}

/// One call as the engine sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    pub tool: String,
    pub risk: Risk,
    /// The whole input.
    pub input: Value,
    /// The workspace root, when the session has one.
    pub workspace: Option<PathBuf>,
    /// Every path the input names (`path`, `cwd`, `paths`); a tool
    /// whose schema has `path`/`cwd` and an input without it names the
    /// root.
    pub paths: Vec<SubjectPath>,
    /// `command`, whole.
    pub command: Option<String>,
    /// One line for the prompt.
    pub description: String,
}

impl PermissionRequest {
    /// Builds the request of one call.
    pub fn for_call(spec: &ToolSpec, input: &Value, workspace: Option<&Workspace>) -> Self {
        let mut given: Vec<String> = Vec::new();
        for key in PATH_KEYS {
            match input.get(key) {
                Some(Value::String(p)) => given.push(p.clone()),
                Some(_) => {}
                None => {
                    if spec.input_schema["properties"].get(key).is_some() {
                        given.push(".".to_owned());
                    }
                }
            }
        }
        if let Some(Value::Array(items)) = input.get("paths") {
            given.extend(items.iter().filter_map(|v| v.as_str().map(str::to_owned)));
        }
        let paths: Vec<SubjectPath> = given.into_iter().map(|p| resolve(workspace, p)).collect();
        let command = input
            .get("command")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let description = match input.get("description").and_then(Value::as_str) {
            Some(d) if !d.trim().is_empty() => d.trim().to_owned(),
            _ => match (&command, paths.is_empty()) {
                (Some(c), _) => format!("{}: {c}", spec.name),
                (None, false) => format!(
                    "{}: {}",
                    spec.name,
                    paths
                        .iter()
                        .map(|p| p.shown.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                (None, true) => spec.name.clone(),
            },
        };
        Self {
            tool: spec.name.clone(),
            risk: spec.risk,
            input: input.clone(),
            workspace: workspace.map(|w| w.root().to_path_buf()),
            paths,
            command,
            description,
        }
    }

    /// At least one path is outside the workspace.
    pub fn has_outside(&self) -> bool {
        self.paths.iter().any(|p| p.outside)
    }

    pub fn shown_paths(&self) -> Vec<String> {
        self.paths.iter().map(|p| p.shown.clone()).collect()
    }

    /// The input for the prompt: every top-level string other than a
    /// path, a pattern or the command cut to [`INPUT_VIEW_MAX_CHARS`].
    pub fn input_view(&self) -> Value {
        let mut view = self.input.clone();
        if let Value::Object(fields) = &mut view {
            for (key, value) in fields.iter_mut() {
                if WHOLE_KEYS.contains(&key.as_str()) {
                    continue;
                }
                if let Value::String(s) = value
                    && s.chars().count() > INPUT_VIEW_MAX_CHARS
                {
                    let total = s.chars().count();
                    let mut cut: String = s.chars().take(INPUT_VIEW_MAX_CHARS).collect();
                    let _ = write!(cut, "… [{} more chars]", total - INPUT_VIEW_MAX_CHARS);
                    *value = Value::String(cut);
                }
            }
        }
        view
    }
}

fn resolve(workspace: Option<&Workspace>, given: String) -> SubjectPath {
    let Some(ws) = workspace else {
        return SubjectPath {
            shown: given.replace('\\', "/"),
            given,
            abs: None,
            outside: true,
        };
    };
    match ws.resolve(&given) {
        Ok(abs) => SubjectPath {
            given,
            shown: ws.display(&abs),
            abs: Some(abs),
            outside: false,
        },
        Err(PathError::Outside { path }) => SubjectPath {
            given,
            shown: path.replace('\\', "/"),
            abs: None,
            outside: true,
        },
        Err(_) => SubjectPath {
            shown: given.replace('\\', "/"),
            given,
            abs: None,
            outside: true,
        },
    }
}

/// The rules an answer would write for `req`, most specific first,
/// with `effect = allow` (a `deny_always` answer flips it).
pub fn suggest(req: &PermissionRequest) -> Vec<RuleSpec> {
    let rule = |m: RuleMatch| RuleSpec {
        tool: req.tool.clone(),
        effect: RuleEffect::Allow,
        r#match: m,
    };
    let mut out = Vec::new();
    if let Some(command) = req.command.as_deref().map(str::trim)
        && !command.is_empty()
    {
        if rules::is_chain(command) {
            // A prefix never matches a chain: offer this exact command.
            out.push(rule(RuleMatch {
                command_regex: Some(format!("^{}$", regex::escape(command))),
                ..RuleMatch::default()
            }));
        } else {
            let words: Vec<&str> = command.split_whitespace().collect();
            let two = words.iter().take(2).copied().collect::<Vec<_>>().join(" ");
            out.push(rule(RuleMatch {
                command_prefix: Some(two.clone()),
                ..RuleMatch::default()
            }));
            if words.len() > 1 && words[0] != two {
                out.push(rule(RuleMatch {
                    command_prefix: Some(words[0].to_owned()),
                    ..RuleMatch::default()
                }));
            }
        }
        return out;
    }
    if let Some(first) = req.paths.first() {
        let dir = match first.shown.rsplit_once('/') {
            Some((dir, _)) => format!("{dir}/**"),
            None => "**".to_owned(),
        };
        let outside = first.outside.then_some(true);
        out.push(rule(RuleMatch {
            path: Some(dir.clone()),
            outside_workspace: outside,
            ..RuleMatch::default()
        }));
        if first.shown != "." && first.shown != dir {
            out.push(rule(RuleMatch {
                path: Some(first.shown.clone()),
                outside_workspace: outside,
                ..RuleMatch::default()
            }));
        }
        return out;
    }
    out.push(rule(RuleMatch::default()));
    out
}

/// What a prompt came back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Answered {
        answer: PermissionAnswer,
        /// The rule to write instead of the suggested one.
        rule: Option<RuleSpec>,
    },
    TimedOut,
    /// Nobody is attached to ask.
    NoClient,
}

/// One prompt.
#[derive(Debug, Clone)]
pub struct Ask<'a> {
    pub request_id: &'a str,
    pub agent_id: &'a AgentId,
    pub request: &'a PermissionRequest,
    pub suggested: &'a [RuleSpec],
    pub timeout: Duration,
}

/// Asks the user. [`AgentPrompter`] does it over RPC; [`NoClient`]
/// answers as if nobody were attached.
#[async_trait]
pub trait Prompter: Send + Sync {
    async fn ask(&self, ask: &Ask<'_>) -> Reply;
}

/// A prompter with nobody behind it: every ask is headless.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoClient;

#[async_trait]
impl Prompter for NoClient {
    async fn ask(&self, _: &Ask<'_>) -> Reply {
        Reply::NoClient
    }
}

/// The `allow_session` answers of one session: allow rules held in
/// memory, consulted only for calls the files would ask about.
#[derive(Debug, Default)]
pub struct SessionRules {
    rules: Mutex<Vec<CompiledRule>>,
}

impl SessionRules {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<CompiledRule>> {
        self.rules
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Remembers an allow rule; returns its 1-based index.
    ///
    /// # Errors
    /// The rule does not compile.
    pub fn add(&self, rule: RuleSpec) -> Result<usize, String> {
        let compiled = CompiledRule::compile(RuleSpec {
            effect: RuleEffect::Allow,
            ..rule
        })?;
        let mut rules = self.lock();
        rules.push(compiled);
        Ok(rules.len())
    }

    pub fn first_match(&self, req: &PermissionRequest) -> Option<usize> {
        self.lock()
            .iter()
            .position(|r| r.matches(req))
            .map(|i| i + 1)
    }

    pub fn list(&self) -> Vec<RuleSpec> {
        self.lock().iter().map(|r| r.spec.clone()).collect()
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }
}

/// A rule an answer wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleWritten {
    pub path: PathBuf,
    pub line: u64,
    pub rule: RuleSpec,
}

/// How one call was decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub decision: PermissionDecision,
    pub source: PermissionSource,
    pub rule_ref: Option<String>,
    /// What the mentor reads when denied.
    pub reason: Option<String>,
    /// Set when a prompt was shown.
    pub request_id: Option<String>,
    pub answer: Option<PermissionAnswer>,
    /// Time spent waiting for the answer.
    pub waited: Duration,
    pub rule_written: Option<RuleWritten>,
    /// Something worth telling the user (a rule that could not be
    /// written, a file not in force).
    pub notes: Vec<String>,
}

impl Outcome {
    fn allow(source: PermissionSource, rule_ref: Option<String>) -> Self {
        Self {
            decision: PermissionDecision::Allow,
            source,
            rule_ref,
            reason: None,
            request_id: None,
            answer: None,
            waited: Duration::ZERO,
            rule_written: None,
            notes: Vec::new(),
        }
    }

    fn deny(source: PermissionSource, rule_ref: Option<String>, reason: String) -> Self {
        Self {
            decision: PermissionDecision::Deny,
            reason: Some(reason),
            ..Self::allow(source, rule_ref)
        }
    }

    pub fn is_allowed(&self) -> bool {
        self.decision == PermissionDecision::Allow
    }
}

/// The engine of one session: its rule files, its config, its
/// remembered answers.
pub struct Engine {
    config: PermissionsConfig,
    user: Mutex<Layer>,
    workspace: Option<Mutex<Layer>>,
    session: Arc<SessionRules>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("config", &self.config)
            .field("session_rules", &self.session.len())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Loads the user file under `paths` and the workspace's, when
    /// there is one.
    pub fn new(
        paths: &Paths,
        workspace: Option<&Workspace>,
        config: PermissionsConfig,
        session: Arc<SessionRules>,
    ) -> Self {
        Self::with_files(
            user_rules_file(paths),
            workspace.map(Workspace::permissions_file),
            config,
            session,
        )
    }

    /// The engine over explicit files.
    pub fn with_files(
        user_file: PathBuf,
        workspace_file: Option<PathBuf>,
        config: PermissionsConfig,
        session: Arc<SessionRules>,
    ) -> Self {
        Self {
            config,
            user: Mutex::new(Layer::load(RuleSource::User, user_file)),
            workspace: workspace_file.map(|p| Mutex::new(Layer::load(RuleSource::Workspace, p))),
            session,
        }
    }

    pub fn config(&self) -> &PermissionsConfig {
        &self.config
    }

    pub fn session_rules(&self) -> &Arc<SessionRules> {
        &self.session
    }

    pub fn ask_timeout(&self) -> Duration {
        Duration::from_secs(self.config.ask_timeout_s.max(1))
    }

    /// Runs `f` over the layers, highest first, re-read when changed.
    fn with_layers<T>(&self, f: impl FnOnce(&[&Layer]) -> T) -> T {
        let mut user = self
            .user
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        user.reload_if_changed();
        match &self.workspace {
            Some(ws) => {
                let mut ws = ws.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                ws.reload_if_changed();
                f(&[&ws, &user])
            }
            None => f(&[&user]),
        }
    }

    /// What the files (and built-ins) say, without asking anyone.
    pub fn evaluate(&self, req: &PermissionRequest) -> rules::Evaluation {
        self.with_layers(|layers| rules::evaluate(layers, req))
    }

    /// Decides one call; see the module docs for the order.
    pub async fn decide(
        &self,
        req: &PermissionRequest,
        agent: &AgentId,
        mode: PermissionMode,
        prompter: &dyn Prompter,
    ) -> Outcome {
        let mutating = matches!(req.risk, Risk::Write | Risk::Execute);
        if mode == PermissionMode::Plan && mutating {
            return Outcome::deny(
                PermissionSource::Mode,
                Some("mode:plan".to_owned()),
                format!(
                    "denied: plan mode is read-only and {} is a {} tool",
                    req.tool,
                    rules::risk_name(req.risk)
                ),
            );
        }
        let eval = self.evaluate(req);
        match eval.effect {
            RuleEffect::Allow => return Outcome::allow(PermissionSource::Rule, eval.rule_ref),
            RuleEffect::Deny => {
                let reason = match (&eval.rule_ref, &eval.rule) {
                    (Some(r), Some(rule)) => format!("denied by rule {r} ({rule})"),
                    (Some(r), None) => format!("denied by {r}"),
                    _ => "denied by rule".to_owned(),
                };
                return Outcome::deny(PermissionSource::Rule, eval.rule_ref, reason);
            }
            RuleEffect::Ask | _ => {}
        }
        if let Some(i) = self.session.first_match(req) {
            return Outcome::allow(PermissionSource::Session, Some(format!("session:{i}")));
        }
        if mode == PermissionMode::Auto && req.risk == Risk::Write && !req.has_outside() {
            return Outcome::allow(PermissionSource::Mode, Some("mode:auto".to_owned()));
        }

        let request_id = crate::trace::CallId::generate().to_string();
        let suggested = suggest(req);
        let timeout = self.ask_timeout();
        let started = Instant::now();
        let reply = prompter
            .ask(&Ask {
                request_id: &request_id,
                agent_id: agent,
                request: req,
                suggested: &suggested,
                timeout,
            })
            .await;
        let waited = started.elapsed();
        let mut outcome = match reply {
            Reply::Answered { answer, rule } => {
                self.answered(req, answer, rule.as_ref(), suggested.first(), &request_id)
            }
            Reply::TimedOut => Outcome::deny(
                PermissionSource::Timeout,
                None,
                format!(
                    "denied: nobody answered the permission request within {} s",
                    timeout.as_secs()
                ),
            ),
            Reply::NoClient => match self.config.headless {
                Headless::AllowReadonly if req.risk == Risk::ReadOnly => {
                    Outcome::allow(PermissionSource::Headless, None)
                }
                Headless::AllowReadonly => Outcome::deny(
                    PermissionSource::Headless,
                    None,
                    format!(
                        "denied: no client attached to ask and only read-only calls run headless ({} is {})",
                        req.tool,
                        rules::risk_name(req.risk)
                    ),
                ),
                Headless::Deny => Outcome::deny(
                    PermissionSource::Headless,
                    None,
                    "denied: no client attached to ask (permissions.headless = \"deny\")"
                        .to_owned(),
                ),
            },
        };
        outcome.request_id = Some(request_id);
        outcome.waited = waited;
        outcome.notes.extend(
            eval.broken
                .iter()
                .map(|e| format!("rules file not in force: {e}")),
        );
        outcome
    }

    fn answered(
        &self,
        req: &PermissionRequest,
        answer: PermissionAnswer,
        rule: Option<&RuleSpec>,
        suggested: Option<&RuleSpec>,
        request_id: &str,
    ) -> Outcome {
        let user = PermissionSource::User;
        let chosen = |effect: RuleEffect| {
            rule.or(suggested)
                .cloned()
                .map(|r| RuleSpec { effect, ..r })
        };
        let mut outcome = match answer {
            PermissionAnswer::AllowOnce => Outcome::allow(user, None),
            PermissionAnswer::AllowSession => {
                let mut o = Outcome::allow(user, None);
                match chosen(RuleEffect::Allow).map(|r| self.session.add(r)) {
                    Some(Ok(i)) => o.rule_ref = Some(format!("session:{i}")),
                    Some(Err(e)) => o.notes.push(format!("session rule not kept: {e}")),
                    None => {}
                }
                o
            }
            PermissionAnswer::AllowWorkspace | PermissionAnswer::AllowAlways => {
                let mut o = Outcome::allow(user, None);
                let target = if answer == PermissionAnswer::AllowWorkspace {
                    RuleSource::Workspace
                } else {
                    RuleSource::User
                };
                self.write_rule(&mut o, req, target, chosen(RuleEffect::Allow), request_id);
                o
            }
            PermissionAnswer::DenyOnce => Outcome::deny(user, None, "denied by user".to_owned()),
            PermissionAnswer::DenyAlways => {
                let mut o = Outcome::deny(user, None, "denied by user".to_owned());
                self.write_rule(
                    &mut o,
                    req,
                    RuleSource::User,
                    chosen(RuleEffect::Deny),
                    request_id,
                );
                o
            }
            _ => Outcome::deny(user, None, "denied: unknown answer".to_owned()),
        };
        outcome.answer = Some(answer);
        outcome
    }

    /// Appends `rule` to the file of `target` and reloads that layer;
    /// a failure is a note, the decision stands as answered.
    fn write_rule(
        &self,
        outcome: &mut Outcome,
        req: &PermissionRequest,
        target: RuleSource,
        rule: Option<RuleSpec>,
        request_id: &str,
    ) {
        let Some(rule) = rule else {
            return;
        };
        let layer = if target == RuleSource::Workspace {
            let Some(layer) = &self.workspace else {
                outcome
                    .notes
                    .push("no workspace to write the rule to; allowed once".to_owned());
                return;
            };
            layer
        } else {
            &self.user
        };
        let mut layer = layer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let comment = format!(
            "{} for {} (request {request_id})",
            match rule.effect {
                RuleEffect::Deny => "deny_always",
                _ if target == RuleSource::Workspace => "allow_workspace",
                _ => "allow_always",
            },
            req.description
        );
        match rules::append_rule(&layer.path, &rule, &comment) {
            Ok(line) => {
                debug!(file = %layer.path.display(), line, "permission rule written");
                layer.reload();
                outcome.rule_written = Some(RuleWritten {
                    path: layer.path.clone(),
                    line,
                    rule,
                });
            }
            Err(e) => {
                warn!(error = %e, "cannot write permission rule");
                outcome.notes.push(format!("rule not written: {e}"));
            }
        }
    }

    /// The rules and files as `tools.rules` lists them (built-ins
    /// last).
    pub fn listing(
        &self,
    ) -> (
        Vec<apprentice_api::types::RuleInfo>,
        Vec<apprentice_api::types::RuleFileInfo>,
    ) {
        self.with_layers(|layers| {
            let mut rules = Vec::new();
            let mut files = Vec::new();
            for layer in layers {
                rules.extend(layer.list());
                files.push(layer.info());
            }
            rules.extend(rules::list_builtins());
            (rules, files)
        })
    }

    /// The path of the `target` file, when there is one.
    pub fn file_of(&self, target: RuleSource) -> Option<PathBuf> {
        match target {
            RuleSource::Workspace => self.workspace.as_ref().map(|l| {
                l.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .path
                    .clone()
            }),
            _ => Some(
                self.user
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .path
                    .clone(),
            ),
        }
    }
}

/// `path` for `Path`-typed callers of [`user_rules_file`].
pub fn workspace_rules_file(root: &Path) -> PathBuf {
    root.join(crate::workspace::HARNESS_DIR)
        .join(crate::workspace::PERMISSIONS_FILE)
}

#[cfg(test)]
mod tests;
