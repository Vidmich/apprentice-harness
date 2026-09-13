//! The RPC method set. Each method is a zero-sized type implementing
//! [`Method`], which ties the wire name to its params and result types.
//!
//! Adding a method is additive; changing an existing shape is a breaking
//! change and bumps [`crate::API_VERSION`].

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::types::{
    ConfigLayer, ConfigSource, EventSummary, PermissionAnswer, RuleFileInfo, RuleInfo, RuleMatch,
    RuleSpec, RunOptions, SessionSummary, TokenStats, ToolInfo, TraceEvent, WorkspaceSummary,
};

/// A typed RPC method.
pub trait Method {
    /// Wire name, e.g. `"daemon.hello"`.
    const NAME: &'static str;
    type Params: Serialize + DeserializeOwned + Send + 'static;
    type Result: Serialize + DeserializeOwned + Send + 'static;
}

/// Results that carry a subscription id (streaming methods).
pub trait HasSubscription {
    fn subscription(&self) -> &str;
}

/// Params/result for methods that take or return nothing. Serialises as `{}`;
/// `null`/absent params are accepted by the router.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Empty {}

macro_rules! method {
    ($(#[$meta:meta])* $name:ident, $wire:literal, $params:ty, $result:ty) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy)]
        pub struct $name;
        impl Method for $name {
            const NAME: &'static str = $wire;
            type Params = $params;
            type Result = $result;
        }
    };
}

// ---------------------------------------------------------------- daemon.*

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloParams {
    pub client: String,
    pub client_version: String,
    pub api_version: u32,
    /// Token from `daemon.json`. Optional for transports that need no auth
    /// (stdio); the router decides whether it is required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloResult {
    pub daemon_version: String,
    pub api_version: u32,
    pub pid: u32,
}

method!(
    /// Handshake; must be the first request on a connection.
    DaemonHello, "daemon.hello", HelloParams, HelloResult
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatusResult {
    pub version: String,
    pub pid: u32,
    pub uptime_s: u64,
    pub sessions_open: u64,
    pub data_dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_file: Option<String>,
}

method!(DaemonStatus, "daemon.status", Empty, DaemonStatusResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShutdownParams {
    #[serde(default = "default_true")]
    pub graceful: bool,
    /// The daemon token. Lets a client that failed `daemon.hello` with
    /// `incompatible_api` still ask an older daemon to stop, so it can be
    /// replaced; ignored on an authenticated connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl Default for ShutdownParams {
    fn default() -> Self {
        Self {
            graceful: true,
            token: None,
        }
    }
}

fn default_true() -> bool {
    true
}

method!(DaemonShutdown, "daemon.shutdown", ShutdownParams, Empty);

// ---------------------------------------------------------------- config.*

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ConfigGetParams {
    /// Dotted key; `None` returns the whole resolved config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigGetResult {
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ConfigSource>,
}

method!(ConfigGet, "config.get", ConfigGetParams, ConfigGetResult);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigSetParams {
    pub key: String,
    pub value: Value,
    pub layer: ConfigLayer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

method!(ConfigSet, "config.set", ConfigSetParams, Empty);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigPathResult {
    pub config_file: String,
    pub data_dir: String,
}

method!(ConfigPath, "config.path", Empty, ConfigPathResult);

// ---------------------------------------------------------------- auth.*

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthSetKeyParams {
    pub provider: String,
    pub key: String,
}

method!(AuthSetKey, "auth.set_key", AuthSetKeyParams, Empty);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAuth {
    pub name: String,
    pub configured: bool,
    /// `env` | `keychain` | `file`, when configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthStatusResult {
    pub providers: Vec<ProviderAuth>,
}

method!(AuthStatus, "auth.status", Empty, AuthStatusResult);

// ---------------------------------------------------------------- session.*

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SessionCreateParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCreateResult {
    pub session_id: String,
}

method!(
    SessionCreate,
    "session.create",
    SessionCreateParams,
    SessionCreateResult
);

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SessionListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionListResult {
    pub sessions: Vec<SessionSummary>,
}

method!(
    SessionList,
    "session.list",
    SessionListParams,
    SessionListResult
);

// ---------------------------------------------------------------- agent.*

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRunParams {
    pub session_id: String,
    pub prompt: String,
    #[serde(default)]
    pub options: RunOptions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRunResult {
    pub agent_id: String,
    pub subscription: String,
}

impl HasSubscription for AgentRunResult {
    fn subscription(&self) -> &str {
        &self.subscription
    }
}

method!(
    /// Starts an agent; events stream on the returned subscription.
    AgentRun, "agent.run", AgentRunParams, AgentRunResult
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentIdParams {
    pub agent_id: String,
}

method!(AgentCancel, "agent.cancel", AgentIdParams, Empty);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSubscribeResult {
    pub subscription: String,
    /// Whether the agent is still running (events will follow).
    pub running: bool,
}

impl HasSubscription for AgentSubscribeResult {
    fn subscription(&self) -> &str {
        &self.subscription
    }
}

method!(
    /// Re-attach to a running agent's event stream (no replay of past events).
    AgentSubscribe, "agent.subscribe", AgentIdParams, AgentSubscribeResult
);

// ---------------------------------------------------------------- trace.*

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TraceListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Return events with `seq` lower than this (paging backwards).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceListResult {
    pub events: Vec<EventSummary>,
}

method!(TraceList, "trace.list", TraceListParams, TraceListResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceGetParams {
    pub event_id: String,
    #[serde(default)]
    pub include_blob: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceGetResult {
    pub event: TraceEvent,
    /// Blob content as UTF-8 text (lossy for binary), when requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

method!(TraceGet, "trace.get", TraceGetParams, TraceGetResult);

// ---------------------------------------------------------------- stats.*

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StatsTokensParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

method!(StatsTokens, "stats.tokens", StatsTokensParams, TokenStats);

/// Recomputes `cost_micros` of stored mentor calls from the current pricing
/// table. All filters are optional; times compare against the call start.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StatsRepriceParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StatsRepriceResult {
    /// Completed calls with usage that matched the filter.
    pub examined: u64,
    /// Rows whose stored cost differed from the recomputed one.
    pub changed: u64,
    /// Matching calls whose model has no pricing entry (cost left `NULL`).
    pub unpriced: u64,
}

method!(
    StatsReprice,
    "stats.reprice",
    StatsRepriceParams,
    StatsRepriceResult
);

// ---------------------------------------------------------------- tools.*

/// `tools.list`: every registered tool with whether the config for
/// `workspace` (`tools.disabled`) leaves it enabled.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ToolsListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolsListResult {
    /// Sorted by name.
    pub tools: Vec<ToolInfo>,
}

method!(ToolsList, "tools.list", ToolsListParams, ToolsListResult);

/// `tools.rules`: the permission rules in force for `workspace` (task
/// M01-07), in precedence order: workspace file, user file, built-ins.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ToolsRulesParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsRulesResult {
    pub rules: Vec<RuleInfo>,
    /// The workspace file (when a workspace was given) and the user file.
    pub files: Vec<RuleFileInfo>,
}

method!(
    ToolsRules,
    "tools.rules",
    ToolsRulesParams,
    ToolsRulesResult
);

/// `tools.allow` / `tools.deny`: appends a rule to a rules file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsRuleParams {
    /// Exact tool name or `*`.
    pub tool: String,
    #[serde(default, rename = "match", skip_serializing_if = "RuleMatch::is_empty")]
    pub r#match: RuleMatch,
    pub layer: ConfigLayer,
    /// Required for the workspace layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsRuleResult {
    /// The file written.
    pub path: String,
    /// Line of the new `[[rule]]` header.
    pub line: u64,
    pub rule: RuleSpec,
}

method!(ToolsAllow, "tools.allow", ToolsRuleParams, ToolsRuleResult);
method!(ToolsDeny, "tools.deny", ToolsRuleParams, ToolsRuleResult);

// ------------------------------------------------------------ permission.*

/// `permission.respond`: answers a `permission.request` event. The
/// first answer wins; a later one (or one for an expired request) is
/// `not_found`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRespondParams {
    pub request_id: String,
    pub answer: PermissionAnswer,
    /// The rule to write for `allow_workspace`, `allow_always` and
    /// `deny_always` (default: the first suggested one, with the
    /// answer's effect).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<RuleSpec>,
}

method!(
    PermissionRespond,
    "permission.respond",
    PermissionRespondParams,
    Empty
);

// ------------------------------------------------------------- workspace.*

/// `workspace.add`: registers a root directory (idempotent: the same
/// root returns the same id) and marks it used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceAddParams {
    /// Absolute path of the root; canonicalised by the daemon.
    pub root: String,
    /// Display name for a new row (default: the directory name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

method!(
    WorkspaceAdd,
    "workspace.add",
    WorkspaceAddParams,
    WorkspaceSummary
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceListResult {
    /// Most recently used first.
    pub workspaces: Vec<WorkspaceSummary>,
}

method!(WorkspaceList, "workspace.list", Empty, WorkspaceListResult);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceIdParams {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRemoveResult {
    /// Sessions that pointed at the row and now only keep their
    /// historical `workspace` path.
    pub sessions_unlinked: u64,
}

method!(
    /// Forgets a workspace. Files and traces are untouched.
    WorkspaceRemove, "workspace.remove", WorkspaceIdParams, WorkspaceRemoveResult
);

/// `workspace.info` / `workspace.refresh`: the registry row plus what
/// the daemon knows about the tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // wire shape: independent facts, not a state machine
pub struct WorkspaceInfoResult {
    pub id: String,
    pub root: String,
    pub name: String,
    pub created_at: String,
    pub last_used_at: String,
    /// Files in the index (ignore rules applied).
    pub file_count: u64,
    /// The tree has more files than the index holds.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub index_truncated: bool,
    /// Seconds since the index was built.
    pub index_age_s: u64,
    /// Commit hash of `HEAD` when the root is a git work tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_head: Option<String>,
    /// Checked-out branch, when `HEAD` is symbolic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    /// `.harness/HARNESS.md` exists.
    pub has_instructions: bool,
    /// `.harness/config.toml` exists.
    pub has_config: bool,
    /// `.harness/ignore` exists.
    pub has_ignore_file: bool,
    /// Dotted config keys the workspace config layer overrides.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub config_overrides: Vec<String>,
}

method!(
    WorkspaceInfo,
    "workspace.info",
    WorkspaceIdParams,
    WorkspaceInfoResult
);

method!(
    /// Re-reads `.harness/ignore` and rebuilds the file index now.
    WorkspaceRefresh, "workspace.refresh", WorkspaceIdParams, WorkspaceInfoResult
);

/// Every method name known to this API version, for parity checks and
/// documentation.
pub const ALL_METHODS: &[&str] = &[
    DaemonHello::NAME,
    DaemonStatus::NAME,
    DaemonShutdown::NAME,
    ConfigGet::NAME,
    ConfigSet::NAME,
    ConfigPath::NAME,
    AuthSetKey::NAME,
    AuthStatus::NAME,
    SessionCreate::NAME,
    SessionList::NAME,
    AgentRun::NAME,
    AgentCancel::NAME,
    AgentSubscribe::NAME,
    TraceList::NAME,
    TraceGet::NAME,
    StatsTokens::NAME,
    StatsReprice::NAME,
    ToolsList::NAME,
    ToolsRules::NAME,
    ToolsAllow::NAME,
    ToolsDeny::NAME,
    PermissionRespond::NAME,
    WorkspaceAdd::NAME,
    WorkspaceList::NAME,
    WorkspaceRemove::NAME,
    WorkspaceInfo::NAME,
    WorkspaceRefresh::NAME,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_names_are_unique_and_namespaced() {
        let mut seen = std::collections::HashSet::new();
        for m in ALL_METHODS {
            assert!(m.contains('.'), "{m} must be <area>.<verb>");
            assert!(seen.insert(*m), "duplicate method {m}");
        }
    }

    #[test]
    fn empty_params_accept_object() {
        let e: Empty = serde_json::from_str("{}").unwrap();
        assert_eq!(e, Empty {});
        assert_eq!(serde_json::to_string(&Empty {}).unwrap(), "{}");
    }
}
