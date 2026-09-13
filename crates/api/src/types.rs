//! Shared data types used by method params/results and events.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Mentor effort level (`output_config.effort`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Effort {
    Low,
    Medium,
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    Max,
}

/// Token usage of one mentor call, exactly as reported by the API.
/// Missing fields deserialise as 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

/// Where a configuration value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConfigSource {
    Default,
    User,
    Workspace,
    Env,
}

/// Configuration layer a value is written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConfigLayer {
    User,
    Workspace,
}

/// Session row as listed by `session.list` (task M01-10 added the
/// activity and cost columns).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    /// The workspace root the session was created on.
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// `open` | `archived` | `deleted`.
    #[serde(default = "default_session_status")]
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    /// Messages in the stored conversation.
    #[serde(default)]
    pub message_count: u64,
    /// When the last message was stored; the creation time before the
    /// first.
    #[serde(default)]
    pub last_activity: String,
    /// How the last agent on the session ended; absent before the
    /// first run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_agent_status: Option<crate::events::AgentStatus>,
    /// Token totals over the session's mentor calls.
    #[serde(default)]
    pub usage: Usage,
    /// Cost of those calls; absent when one had no price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

fn default_session_status() -> String {
    "open".to_owned()
}

/// A session with its resume metadata, as `session.get` and
/// `session.export` return it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    #[serde(flatten)]
    pub summary: SessionSummary,
    /// `user` | `prompt` | `generated`; absent without a title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_source: Option<String>,
    /// The system prompt version and the tool-set hash the session
    /// started under (set at its first run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_hash: Option<String>,
    /// The config snapshot taken at creation.
    pub config: Value,
}

/// One stored message of a conversation: the content blocks exactly as
/// the mentor sent or received them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMessage {
    /// Position in the conversation, from 1.
    pub seq: u64,
    /// `user` | `assistant` | `system`.
    pub role: String,
    /// The content-block array.
    pub content: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    pub created_at: String,
}

/// A `session.search` hit: one message whose text matched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSearchHit {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    pub seq: u64,
    pub role: String,
    /// The matching text with the matches in `[` `]`, cut with `…`.
    pub snippet: String,
    pub created_at: String,
}

/// One mentor call of a session, as `session.export` lists them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MentorCallInfo {
    pub id: String,
    pub agent_id: String,
    pub step_id: String,
    /// `step` | `title`.
    pub kind: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// `running` | `ok` | `cancelled` | `error`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_byte_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_ms: Option<u64>,
}

/// Current value of [`SessionExport::format`].
pub const SESSION_EXPORT_FORMAT: &str = "harness-session/1";

/// A session as one JSON document (`session.export`): enough to list,
/// resume and account for it in another store. Traces are not included
/// (a trace bundle is the M01-14 export).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionExport {
    /// [`SESSION_EXPORT_FORMAT`].
    pub format: String,
    pub exported_at: String,
    pub session: SessionInfo,
    pub messages: Vec<SessionMessage>,
    pub mentor_calls: Vec<MentorCallInfo>,
}

/// A registered workspace, as returned by `workspace.add` and listed by
/// `workspace.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSummary {
    pub id: String,
    /// Canonical absolute root path.
    pub root: String,
    pub name: String,
    pub created_at: String,
    pub last_used_at: String,
}

/// Trace event row as listed by `trace.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSummary {
    pub id: String,
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub step_id: Option<String>,
    pub seq: u64,
    pub ts: String,
    pub kind: String,
    /// Size of the attached blob, if any.
    #[serde(default)]
    pub blob_bytes: Option<u64>,
}

/// Full trace event as returned by `trace.get`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceEvent {
    #[serde(flatten)]
    pub summary: EventSummary,
    pub payload: Value,
    #[serde(default)]
    pub blob_id: Option<String>,
}

/// Options for `agent.run`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RunOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// `None` = use config (`apprentice.enabled`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apprentice: Option<bool>,
    /// `None` = use config (`permissions.default_mode`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
}

/// How the permission engine treats a run (task M01-07).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PermissionMode {
    /// Rules decide; the client is asked for the rest.
    #[default]
    #[serde(alias = "ask")]
    Default,
    /// Read-only: every `write` and `execute` call is denied.
    Plan,
    /// `ask` counts as `allow` for writes inside the workspace; execute,
    /// network and anything outside are still asked.
    Auto,
}

/// A client's answer to a `permission.request`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PermissionAnswer {
    AllowOnce,
    /// Allow this and matching calls for the rest of the session.
    AllowSession,
    /// Write an allow rule to the workspace's `permissions.toml`.
    AllowWorkspace,
    /// Write an allow rule to the user's `permissions.toml`.
    AllowAlways,
    DenyOnce,
    /// Write a deny rule to the user's `permissions.toml`.
    DenyAlways,
}

/// What the engine decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PermissionDecision {
    Allow,
    Deny,
}

/// Who decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PermissionSource {
    /// A rule from a file, a built-in, or the file's `default`.
    Rule,
    /// The client's answer to a prompt.
    User,
    /// An earlier `allow_session` answer in this session.
    Session,
    /// The run's `permission_mode` (`plan`, `auto`).
    Mode,
    /// Nobody answered within `permissions.ask_timeout_s`.
    Timeout,
    /// No client was attached; `permissions.headless` decided.
    Headless,
}

/// What a rule does when it matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RuleEffect {
    Allow,
    Deny,
    Ask,
}

/// Conditions of a rule; every one given must hold.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleMatch {
    /// Glob on the root-relative path of every path the call names
    /// (`src/**`, `**` for anything under the root). A path outside
    /// the workspace is matched, as an absolute `/` path, only by a
    /// rule with `outside_workspace = true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The command (trimmed) is this, or starts with this followed by
    /// whitespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_prefix: Option<String>,
    /// A regular expression found anywhere in the command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_regex: Option<String>,
    /// `true` matches only calls naming a path outside the workspace,
    /// `false` only calls that name none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outside_workspace: Option<bool>,
    /// The tool's risk class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<crate::events::Risk>,
}

impl RuleMatch {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// One permission rule, as in `permissions.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSpec {
    /// Exact tool name or `*`.
    pub tool: String,
    pub effect: RuleEffect,
    #[serde(default, rename = "match", skip_serializing_if = "RuleMatch::is_empty")]
    pub r#match: RuleMatch,
}

/// Where a rule lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RuleSource {
    Workspace,
    User,
    Builtin,
}

/// A rule as `tools.rules` lists it, with where it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleInfo {
    pub source: RuleSource,
    /// 1-based position within its source; `source:index` is the
    /// `rule_ref` of decision events.
    pub index: usize,
    /// Line of the `[[rule]]` header in the file, for file rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    /// Built-in rules have a name instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub rule: RuleSpec,
}

/// What a rules file says for calls no rule matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RuleDefault {
    #[default]
    Ask,
    Deny,
}

/// One rules file as `tools.rules` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleFileInfo {
    pub source: RuleSource,
    pub path: String,
    pub exists: bool,
    /// The file's `default`, when it exists and parses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<RuleDefault>,
    /// Why the file is not in force (parse error, file and line named).
    /// Every call is then asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Aggregated token statistics (see task M00-07).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenStats {
    pub range: StatsRange,
    /// IANA name or fixed offset of the timezone used for `by_day`.
    pub tz: String,
    pub totals: TokenBucket,
    #[serde(default)]
    pub by_model: Vec<TokenBucket>,
    #[serde(default)]
    pub by_day: Vec<TokenBucket>,
    #[serde(default)]
    pub by_session: Vec<TokenBucket>,
    pub apprentice: ApprenticeStats,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StatsRange {
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
}

/// One row of aggregated usage. The `key` names the group (model id, day,
/// session id) and is absent for the grand total.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TokenBucket {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub calls: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub cost_usd: f64,
    /// Calls whose model had no pricing entry (their cost is excluded).
    #[serde(default)]
    pub unpriced_calls: u64,
}

/// Apprentice-side counters (filled from M03 on; zero until then).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ApprenticeStats {
    pub invocations: u64,
    pub bypassed: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub estimated_saved_input: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_wire_names() {
        assert_eq!(serde_json::to_string(&Effort::XHigh).unwrap(), "\"xhigh\"");
        assert_eq!(
            serde_json::from_str::<Effort>("\"max\"").unwrap(),
            Effort::Max
        );
    }

    #[test]
    fn usage_missing_fields_default_to_zero() {
        let u: Usage = serde_json::from_str(r#"{"input_tokens": 12}"#).unwrap();
        assert_eq!(u.input_tokens, 12);
        assert_eq!(u.cache_read_input_tokens, 0);
    }
}

/// A tool as `tools.list` reports it (task M01-01).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    /// The text the mentor sees.
    pub description: String,
    /// JSON Schema (draft 2020-12) of the tool's input.
    pub input_schema: Value,
    pub risk: crate::events::Risk,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Timeout override in seconds; absent = the per-risk default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
    /// `false` when `tools.disabled` names it: not offered to the mentor.
    pub enabled: bool,
}
