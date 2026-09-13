//! The RPC method set. Each method is a zero-sized type implementing
//! [`Method`], which ties the wire name to its params and result types.
//!
//! Adding a method is additive; changing an existing shape is a breaking
//! change and bumps [`crate::API_VERSION`].

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::types::{
    AgentSummary, BundleCounts, BundleManifest, CallSummary, ConfigLayer, ConfigSource,
    EventSummary, ImportedSession, PermissionAnswer, ReplayReport, RuleFileInfo, RuleInfo,
    RuleMatch, RuleSpec, RunOptions, SessionExport, SessionInfo, SessionMessage, SessionSearchHit,
    SessionSummary, StatsGroup, TokenStats, ToolInfo, TraceEvent, WorkspaceSummary,
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

/// `session.list`: newest activity first. Archived and deleted
/// sessions are left out unless `include_archived`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SessionListParams {
    /// Keep sessions whose title or messages contain these words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Keep sessions created on this workspace root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Keep sessions linked to this registry row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionListResult {
    pub sessions: Vec<SessionSummary>,
}

method!(
    SessionList,
    "session.list",
    SessionListParams,
    SessionListResult
);

/// `session.get`: the session and a page of its messages, in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionGetParams {
    pub id: String,
    /// Messages with `seq` above this (paging forwards).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<u64>,
    /// Messages with `seq` below this (paging backwards: the newest
    /// page first, then older ones). Wins over `after_seq`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionGetResult {
    pub session: SessionInfo,
    /// The page, oldest first whichever way it was paged.
    pub messages: Vec<SessionMessage>,
    /// More messages follow the page (`after_seq`) or precede it
    /// (`before_seq`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_more: bool,
    /// Every agent of the session, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<AgentSummary>,
}

method!(
    SessionGet,
    "session.get",
    SessionGetParams,
    SessionGetResult
);

/// `session.search`: full-text search over the text of every stored
/// message; words are matched as prefixes, all of them must appear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSearchParams {
    pub query: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSearchResult {
    /// Newest first.
    pub hits: Vec<SessionSearchHit>,
}

method!(
    SessionSearch,
    "session.search",
    SessionSearchParams,
    SessionSearchResult
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionIdParams {
    pub id: String,
}

/// `session.archive`: hides the session from the default list (or
/// brings it back with `archived: false`). `conflict` while an agent
/// runs on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionArchiveParams {
    pub id: String,
    #[serde(default = "default_true")]
    pub archived: bool,
}

method!(
    SessionArchive,
    "session.archive",
    SessionArchiveParams,
    Empty
);

/// `session.delete`: removes the stored conversation. The traces stay
/// (and the row, as `deleted`) unless `purge_traces`, which removes
/// every event, call and agent of the session and the row itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDeleteParams {
    pub id: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub purge_traces: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SessionDeleteResult {
    pub messages_deleted: u64,
    /// Only with `purge_traces`.
    #[serde(default)]
    pub events_deleted: u64,
}

method!(
    SessionDelete,
    "session.delete",
    SessionDeleteParams,
    SessionDeleteResult
);

/// `session.rename`: a title of the user's, which the generator never
/// replaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRenameParams {
    pub id: String,
    pub title: String,
}

method!(SessionRename, "session.rename", SessionRenameParams, Empty);

method!(
    /// The session as one JSON document (`SessionExport`).
    SessionExportMethod, "session.export", SessionIdParams, SessionExport
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

/// `trace.export`: writes the selected sessions with their agents, steps,
/// events, calls, messages and blobs to a bundle at `output` (task
/// M01-14). A path ending in `.tar.zst` is packed; any other is written
/// as a directory (created; must not exist or be empty).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceExportParams {
    /// Absolute path of the bundle to write.
    pub output: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Sessions created at or after this (the `stats` range grammar).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// Every session (required when nothing else selects).
    #[serde(default)]
    pub all: bool,
    /// Run the redaction pass (secrets, user patterns). Default on.
    #[serde(default = "default_true")]
    pub redact: bool,
    /// Also replace the workspace roots and the home directory with
    /// `<WS>` / `<HOME>`.
    #[serde(default)]
    pub redact_paths: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceExportResult {
    /// Where the bundle was written.
    pub path: String,
    pub manifest: BundleManifest,
}

method!(
    TraceExport,
    "trace.export",
    TraceExportParams,
    TraceExportResult
);

/// `trace.import`: reads a bundle (directory or `.tar.zst`) into the
/// store after verifying every blob. Sessions get new ids unless
/// `keep_ids` (then an existing id is a `conflict`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceImportParams {
    /// Absolute path of the bundle.
    pub path: String,
    /// Attach every imported session to this registered workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub into_workspace: Option<String>,
    #[serde(default)]
    pub keep_ids: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceImportResult {
    pub sessions: Vec<ImportedSession>,
    pub counts: BundleCounts,
    /// The bundle was redacted before it was written.
    pub redacted: bool,
    /// Blobs written to the store (the rest were there already).
    pub blobs_written: u64,
}

method!(
    TraceImport,
    "trace.import",
    TraceImportParams,
    TraceImportResult
);

/// `trace.replay_check`: proves the stored `mentor.request` bodies are
/// what a replay needs. Without a selector every call is checked.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TraceReplayCheckParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// Also rebuild each request from the stored conversation and
    /// compare it to the body.
    #[serde(default)]
    pub rebuild: bool,
}

method!(
    TraceReplayCheck,
    "trace.replay_check",
    TraceReplayCheckParams,
    ReplayReport
);

// ---------------------------------------------------------------- stats.*

/// `stats.tokens`: totals and breakdowns over the mentor calls in a
/// range. `group_by` names the breakdowns wanted (task M01-13); empty
/// means [`StatsGroup::DEFAULT`]. `by_session` stays empty for a query
/// limited to one session.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StatsTokensParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Only the sessions of this workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub group_by: Vec<StatsGroup>,
}

method!(StatsTokens, "stats.tokens", StatsTokensParams, TokenStats);

/// `stats.calls`: the individual mentor calls behind the numbers, newest
/// first, paged (task M01-13). Times compare against the call start.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct StatsCallsParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Rows per page; default [`StatsCallsParams::DEFAULT_LIMIT`], at most
    /// [`StatsCallsParams::MAX_LIMIT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Rows to skip (newest first).
    #[serde(default)]
    pub offset: u64,
}

impl StatsCallsParams {
    pub const DEFAULT_LIMIT: u32 = 100;
    pub const MAX_LIMIT: u32 = 1000;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatsCallsResult {
    pub calls: Vec<CallSummary>,
    /// Matching calls in all, for paging.
    pub total: u64,
}

method!(
    StatsCalls,
    "stats.calls",
    StatsCallsParams,
    StatsCallsResult
);

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

/// `tools.remove`: deletes one `[[rule]]` from a rules file, named the
/// way `tools.rules` lists it (`index`, 1-based within its file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsRemoveParams {
    pub layer: ConfigLayer,
    /// Required for the workspace layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    pub index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsRemoveResult {
    /// The file written.
    pub path: String,
    /// The rule that went.
    pub rule: RuleSpec,
}

method!(
    ToolsRemove,
    "tools.remove",
    ToolsRemoveParams,
    ToolsRemoveResult
);

// ---------------------------------------------------------------- prompt.*

/// `prompt.show`: the assembled system prompt (task M01-09) of a
/// session — the blocks it runs under, or would — or of a workspace.
/// Both absent: the prompt of a session without a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PromptShowParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// A workspace root; ignored when `session_id` is given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Ask the mentor's `count_tokens` for the size of the blocks
    /// (needs an API key; a failure is reported in `token_error`).
    #[serde(default)]
    pub count: bool,
}

/// One system block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptBlock {
    pub text: String,
    /// Carries a cache breakpoint in requests.
    pub cache: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptShowResult {
    /// `mentor_system_v1`, ...
    pub version: String,
    pub blocks: Vec<PromptBlock>,
    /// The session whose live conversation the blocks came from, when
    /// one was running or had run in this daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// Input tokens of the blocks (plus one one-word user message the
    /// count needs), when `count` was asked and succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_error: Option<String>,
}

method!(
    PromptShow,
    "prompt.show",
    PromptShowParams,
    PromptShowResult
);

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
    /// The work tree has staged, unstaged or untracked changes; absent
    /// when the root is not a work tree or `git` could not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_dirty: Option<bool>,
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
    SessionGet::NAME,
    SessionSearch::NAME,
    SessionArchive::NAME,
    SessionDelete::NAME,
    SessionRename::NAME,
    SessionExportMethod::NAME,
    AgentRun::NAME,
    AgentCancel::NAME,
    AgentSubscribe::NAME,
    TraceList::NAME,
    TraceGet::NAME,
    TraceExport::NAME,
    TraceImport::NAME,
    TraceReplayCheck::NAME,
    StatsTokens::NAME,
    StatsCalls::NAME,
    StatsReprice::NAME,
    ToolsList::NAME,
    ToolsRules::NAME,
    ToolsAllow::NAME,
    ToolsDeny::NAME,
    ToolsRemove::NAME,
    PromptShow::NAME,
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
