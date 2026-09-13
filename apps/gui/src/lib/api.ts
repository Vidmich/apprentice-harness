// Hand-written mirror of `crates/api` (methods, events, shared types).
// Wire names and field names must match the Rust definitions exactly;
// `api.test.ts` checks the snapshot JSON from `crates/api/tests/snapshots`
// against the guards below so drift is caught in CI.

export const API_VERSION = 1;

// ---------------------------------------------------------------- errors

export interface ErrorData {
  kind: string;
  details?: unknown;
}

export interface RpcError {
  code: number;
  message: string;
  data?: ErrorData;
}

export function isRpcError(v: unknown): v is RpcError {
  if (typeof v !== "object" || v === null) return false;
  const o = v as Record<string, unknown>;
  if (typeof o.code !== "number" || typeof o.message !== "string") return false;
  if (o.data === undefined) return true;
  return (
    typeof o.data === "object" &&
    o.data !== null &&
    typeof (o.data as Record<string, unknown>).kind === "string"
  );
}

export function errorKind(e: RpcError): string | undefined {
  return e.data?.kind;
}

// ---------------------------------------------------------------- shared types

export type Effort = "low" | "medium" | "high" | "xhigh" | "max";

export interface Usage {
  input_tokens: number;
  output_tokens: number;
  cache_read_input_tokens: number;
  cache_creation_input_tokens: number;
}

export type ConfigSource = "default" | "user" | "workspace" | "env";
export type ConfigLayer = "user" | "workspace";

/** `open` | `archived` | `deleted`. */
export type SessionStatus = "open" | "archived" | "deleted";

export interface SessionSummary {
  id: string;
  title?: string | null;
  /** The workspace root the session was created on. */
  workspace?: string | null;
  workspace_id?: string | null;
  status: SessionStatus;
  created_at: string;
  updated_at: string;
  /** Messages in the stored conversation. */
  message_count: number;
  /** When the last message was stored; the creation time before the first. */
  last_activity: string;
  /** How the last agent on the session ended; absent before the first run. */
  last_agent_status?: AgentStatus;
  /** The agent running on the session right now (in the answering daemon). */
  running_agent?: string;
  /** Token totals over the session's mentor calls. */
  usage: Usage;
  /** Cost of those calls; absent when one had no price. */
  cost_usd?: number;
  /** Mentor calls of the session, every kind (title calls included). */
  calls: number;
}

/** A session with its resume metadata (`session.get`, `session.export`). */
export interface SessionInfo extends SessionSummary {
  /** `user` | `prompt` | `generated`; absent without a title. */
  title_source?: string;
  prompt_version?: string;
  tools_hash?: string;
  /** The config snapshot taken at creation. */
  config: unknown;
}

/** How the mentor's thinking is shown (`mentor.thinking_display`). */
export type ThinkingDisplay = "summarized" | "omitted";

/** One agent (run) of a session with what its mentor calls cost. */
export interface AgentSummary {
  id: string;
  status: "running" | AgentStatus;
  started_at: string;
  ended_at?: string;
  /** The model of the agent's step calls; absent before the first. */
  model?: string;
  /** Mentor calls made so far. */
  calls: number;
  usage: Usage;
  /** Cost of those calls; absent when one had no price. */
  cost_usd?: number;
  /** How the run ended when not `ok` (the stored `agent.finished` error). */
  error?: RpcError;
  /** The run's `outcome` events, oldest first. */
  outcomes?: OutcomeInfo[];
}

/** Kinds of `outcome` events. */
export type OutcomeKind =
  | "files_changed"
  | "tests"
  | "build"
  | "user_accept"
  | "user_reject"
  | "task_done"
  | "error"
  | "reverted";

/** One `outcome` event: what a run left behind, machine-readable. */
export interface OutcomeInfo {
  event_id: string;
  kind: OutcomeKind | string;
  /** One line: `12 passed (cargo test)`, `3 files (2 changed, 1 added)`. */
  summary: string;
  /** `true` for a pass / accept / done, `false` for a failure / reject / error / revert; absent without a verdict. */
  ok?: boolean;
  details: unknown;
  at: string;
}

/** A user's verdict on a run (`session.mark`). */
export type SessionMark = "accept" | "reject" | "done";

export interface SessionMarkParams {
  id: string;
  mark: SessionMark;
  note?: string;
  /** Default: the session's last run. */
  agent_id?: string;
}

export interface SessionMarkResult {
  agent_id: string;
  outcome: OutcomeInfo;
}

/** One stored message: the content blocks exactly as the mentor saw them. */
export interface SessionMessage {
  /** Position in the conversation, from 1. */
  seq: number;
  role: "user" | "assistant" | "system";
  /** The content-block array. */
  content: unknown[];
  agent_id?: string;
  step_id?: string;
  created_at: string;
}

export interface SessionSearchHit {
  session_id: string;
  title?: string;
  workspace?: string;
  seq: number;
  role: string;
  /** The matching text with the matches in `[` `]`, cut with `…`. */
  snippet: string;
  created_at: string;
}

export interface MentorCallInfo {
  id: string;
  agent_id: string;
  step_id: string;
  /** `step` | `title`. */
  kind: string;
  model: string;
  effort?: string;
  started_at: string;
  ended_at?: string;
  status: "running" | "ok" | "cancelled" | "error";
  stop_reason?: string;
  usage?: Usage;
  cost_usd?: number;
  first_byte_ms?: number;
  total_ms?: number;
}

export const SESSION_EXPORT_FORMAT = "harness-session/1";

/** A session as one JSON document (`session.export`). */
export interface SessionExport {
  format: string;
  exported_at: string;
  session: SessionInfo;
  messages: SessionMessage[];
  mentor_calls: MentorCallInfo[];
}

export interface EventSummary {
  id: string;
  session_id: string;
  agent_id?: string | null;
  step_id?: string | null;
  seq: number;
  ts: string;
  kind: string;
  blob_bytes?: number | null;
}

export interface TraceEvent extends EventSummary {
  payload: unknown;
  blob_id?: string | null;
}

export interface RunOptions {
  model?: string;
  effort?: Effort;
  apprentice?: boolean;
  /** Absent = `permissions.default_mode` from config. */
  permission_mode?: PermissionMode;
}

/** How the permission engine treats a run (task M01-07). */
export type PermissionMode = "default" | "plan" | "auto";

export type PermissionAnswer =
  "allow_once" | "allow_session" | "allow_workspace" | "allow_always" | "deny_once" | "deny_always";

export type PermissionDecision = "allow" | "deny";

export type PermissionSource = "rule" | "user" | "session" | "mode" | "timeout" | "headless";

export type RuleEffect = "allow" | "deny" | "ask";

/** Conditions of a rule; every one given must hold. */
export interface RuleMatch {
  path?: string;
  command_prefix?: string;
  command_regex?: string;
  outside_workspace?: boolean;
  risk?: Risk;
}

/** One permission rule, as in `permissions.toml`. */
export interface RuleSpec {
  tool: string;
  effect: RuleEffect;
  match?: RuleMatch;
}

export type RuleSource = "workspace" | "user" | "builtin";

export interface RuleInfo {
  source: RuleSource;
  index: number;
  line?: number;
  name?: string;
  rule: RuleSpec;
}

export type RuleDefault = "ask" | "deny";

export interface RuleFileInfo {
  source: RuleSource;
  path: string;
  exists: boolean;
  default?: RuleDefault;
  /** Parse error naming file and line; every call is asked meanwhile. */
  error?: string;
}

export interface StatsRange {
  since?: string | null;
  until?: string | null;
}

export interface TokenBucket {
  key?: string;
  label?: string;
  calls: number;
  input: number;
  output: number;
  cache_read: number;
  cache_creation: number;
  cost_usd: number;
  unpriced_calls: number;
}

export interface ApprenticeStats {
  invocations: number;
  bypassed: number;
  tokens_in: number;
  tokens_out: number;
  estimated_saved_input: number;
}

/** A breakdown `stats.tokens` can be asked for. */
export type StatsGroup = "day" | "model" | "session" | "workspace" | "kind";

/** Only the breakdowns asked for (`group_by`) are filled; the rest stay empty. */
export interface TokenStats {
  range: StatsRange;
  tz: string;
  totals: TokenBucket;
  by_model: TokenBucket[];
  by_day: TokenBucket[];
  by_session: TokenBucket[];
  by_workspace: TokenBucket[];
  by_kind: TokenBucket[];
  apprentice: ApprenticeStats;
}

/** One mentor call as `stats.calls` lists it. */
export interface CallSummary {
  call_id: string;
  started_at: string;
  session_id: string;
  session_title?: string;
  workspace_id?: string;
  agent_id: string;
  /** `step` | `title`. */
  kind: string;
  model: string;
  effort?: string;
  status: "running" | "ok" | "cancelled" | "error";
  stop_reason?: string;
  input: number;
  output: number;
  cache_read: number;
  cache_creation: number;
  /** Absent while running, after a failure, or without a price. */
  cost_usd?: number;
  total_ms?: number;
  /** The `mentor.request` event; its blob is the exact body sent. */
  request_event_id: string;
}

// ---------------------------------------------------------------- methods

// eslint-disable-next-line @typescript-eslint/no-empty-object-type
export type Empty = {};

export interface HelloParams {
  client: string;
  client_version: string;
  api_version: number;
  token?: string;
}

export interface HelloResult {
  daemon_version: string;
  api_version: number;
  pid: number;
}

export interface DaemonStatusResult {
  version: string;
  pid: number;
  uptime_s: number;
  sessions_open: number;
  data_dir: string;
  log_file?: string;
}

export interface ShutdownParams {
  graceful?: boolean;
  token?: string;
}

export interface ConfigGetParams {
  key?: string;
  workspace?: string;
}

export interface ConfigGetResult {
  value: unknown;
  source?: ConfigSource;
}

export interface ConfigSetParams {
  key: string;
  value: unknown;
  layer: ConfigLayer;
  workspace?: string;
}

export interface ConfigPathResult {
  config_file: string;
  data_dir: string;
}

export interface AuthSetKeyParams {
  provider: string;
  key: string;
}

export interface ProviderAuth {
  name: string;
  configured: boolean;
  source?: string;
}

export interface AuthStatusResult {
  providers: ProviderAuth[];
}

export interface SessionCreateParams {
  workspace?: string;
  title?: string;
}

export interface SessionCreateResult {
  session_id: string;
}

export interface SessionListParams {
  /** Keep sessions whose title or messages contain these words. */
  query?: string;
  /** Keep sessions created on this workspace root. */
  workspace?: string;
  workspace_id?: string;
  /** Also archived and deleted sessions. */
  include_archived?: boolean;
  limit?: number;
  offset?: number;
}

export interface SessionListResult {
  sessions: SessionSummary[];
}

export interface SessionGetParams {
  id: string;
  /** Messages with `seq` above this (paging forwards). */
  after_seq?: number;
  /** Messages with `seq` below this (the newest page first); wins over `after_seq`. */
  before_seq?: number;
  limit?: number;
}

export interface SessionGetResult {
  session: SessionInfo;
  /** The page, oldest first whichever way it was paged. */
  messages: SessionMessage[];
  /** More messages follow the page (`after_seq`) or precede it (`before_seq`). */
  has_more?: boolean;
  /** Every agent of the session, oldest first (absent when none ran). */
  agents?: AgentSummary[];
}

export interface SessionSearchParams {
  query: string;
  include_archived?: boolean;
  limit?: number;
}

export interface SessionSearchResult {
  /** Newest first. */
  hits: SessionSearchHit[];
}

export interface SessionIdParams {
  id: string;
}

export interface SessionArchiveParams {
  id: string;
  /** Default true; false reopens. */
  archived?: boolean;
}

export interface SessionDeleteParams {
  id: string;
  /** Also remove every event, call and agent of the session, and the row. */
  purge_traces?: boolean;
}

export interface SessionDeleteResult {
  messages_deleted: number;
  events_deleted?: number;
}

export interface SessionRenameParams {
  id: string;
  title: string;
}

export interface AgentRunParams {
  session_id: string;
  prompt: string;
  options?: RunOptions;
}

export interface AgentRunResult {
  agent_id: string;
  subscription: string;
}

export interface AgentIdParams {
  agent_id: string;
}

export interface AgentSubscribeResult {
  subscription: string;
  running: boolean;
}

export interface TraceListParams {
  session_id?: string;
  agent_id?: string;
  kinds?: string[];
  limit?: number;
  before_seq?: number;
}

export interface TraceListResult {
  events: EventSummary[];
}

export interface TraceGetParams {
  event_id: string;
  include_blob?: boolean;
}

export interface TraceGetResult {
  event: TraceEvent;
  blob?: string;
}

// Trace bundles (task M01-14): the CLI's `trace export | import |
// replay-check`; the GUI only knows the shapes.

export interface BundleSelection {
  session_ids?: string[];
  workspace_id?: string;
  since?: string;
  until?: string;
  all?: boolean;
}

export interface BundleSession {
  id: string;
  title?: string;
  workspace_id?: string;
  created_at: string;
  updated_at: string;
  status: string;
  agents: number;
  events: number;
  mentor_calls: number;
  messages: number;
}

export interface BundleCounts {
  sessions: number;
  workspaces: number;
  agents: number;
  steps: number;
  events: number;
  mentor_calls: number;
  messages: number;
  blobs: number;
  blob_bytes: number;
}

export interface RedactionRule {
  name: string;
  /** `builtin`, `user`, `workspace` or `paths`. */
  source: string;
  matches: number;
}

export interface RedactionReport {
  applied: boolean;
  rules: RedactionRule[];
  replacements: number;
  secrets: number;
  paths: boolean;
  blob_map?: Record<string, string>;
  touched_requests: number;
  replayable: boolean;
}

export interface BundleManifest {
  format_version: number;
  created_at: string;
  harness_version: string;
  schema_version: number;
  selection?: BundleSelection;
  sessions: BundleSession[];
  counts: BundleCounts;
  redaction?: RedactionReport;
}

export interface TraceExportParams {
  /** Absolute; `.tar.zst` packs, anything else is a directory. */
  output: string;
  session_ids?: string[];
  workspace_id?: string;
  since?: string;
  until?: string;
  all?: boolean;
  /** Default true. */
  redact?: boolean;
  redact_paths?: boolean;
}

export interface TraceExportResult {
  path: string;
  manifest: BundleManifest;
}

export interface TraceImportParams {
  path: string;
  into_workspace?: string;
  keep_ids?: boolean;
}

export interface ImportedSession {
  from: string;
  to: string;
}

export interface TraceImportResult {
  sessions: ImportedSession[];
  counts: BundleCounts;
  redacted: boolean;
  blobs_written: number;
}

export interface TraceReplayCheckParams {
  session_id?: string;
  agent_id?: string;
  call_id?: string;
  /** Calls started at or after this (the `stats` range grammar). */
  since?: string;
  until?: string;
  rebuild?: boolean;
}

export type ReplayStatus = "ok" | "failed" | "skipped";

export interface ReplayCall {
  call_id: string;
  session_id: string;
  agent_id: string;
  step_id: string;
  kind: string;
  request_event_id: string;
  status: ReplayStatus;
  /** `blob`, `hash`, `parse`, `canonical`, `rebuild`, in order. */
  checks: string[];
  problems?: string[];
}

export interface ReplayReport {
  calls: ReplayCall[];
  checked: number;
  passed: number;
  failed: number;
  skipped: number;
  rebuild: boolean;
}

export interface StatsTokensParams {
  since?: string;
  until?: string;
  session_id?: string;
  workspace_id?: string;
  /** Empty = model, day and session. */
  group_by?: StatsGroup[];
}

export interface StatsCallsParams {
  since?: string;
  until?: string;
  session_id?: string;
  workspace_id?: string;
  /** Default 100, at most 1000. */
  limit?: number;
  offset?: number;
}

export interface StatsCallsResult {
  /** Newest first. */
  calls: CallSummary[];
  /** Matching calls in all, for paging. */
  total: number;
}

export interface StatsRepriceParams {
  model?: string;
  since?: string;
  until?: string;
  session_id?: string;
}

export interface StatsRepriceResult {
  examined: number;
  changed: number;
  unpriced: number;
}

export interface StatsOutcomesParams {
  since?: string;
  until?: string;
  workspace_id?: string;
}

/** The outcome signals of the runs started in a range. */
export interface OutcomeStats {
  range: StatsRange;
  /** Runs (main agents) started in the range. */
  agents: number;
  /** Runs with a `tests`, `user_accept`, `user_reject` or `task_done` outcome. */
  labelled: number;
  labelled_share: number;
  by_kind?: Record<string, number>;
  tests_passed: number;
  tests_failed: number;
  accepted: number;
  rejected: number;
  done: number;
  errors?: Record<string, number>;
}

// ---------------------------------------------------------------- tools.*

export interface ToolsListParams {
  workspace?: string;
}

export interface ToolInfo {
  name: string;
  description: string;
  input_schema: unknown;
  risk: Risk;
  tags?: string[];
  timeout_s?: number;
  enabled: boolean;
}

export interface ToolsListResult {
  tools: ToolInfo[];
}

export interface ToolsRulesParams {
  workspace?: string;
}

export interface ToolsRulesResult {
  /** Precedence order: workspace file, user file, built-ins. */
  rules: RuleInfo[];
  files: RuleFileInfo[];
}

export interface ToolsRuleParams {
  tool: string;
  match?: RuleMatch;
  layer: ConfigLayer;
  /** Required for the workspace layer. */
  workspace?: string;
}

export interface ToolsRuleResult {
  path: string;
  line: number;
  rule: RuleSpec;
}

/** `tools.remove`: one `[[rule]]` of a file, by the index `tools.rules` lists. */
export interface ToolsRemoveParams {
  layer: ConfigLayer;
  /** Required for the workspace layer. */
  workspace?: string;
  index: number;
}

export interface ToolsRemoveResult {
  path: string;
  rule: RuleSpec;
}

// ---------------------------------------------------------------- prompt.*

export interface PromptShowParams {
  session_id?: string;
  /** Ignored when `session_id` is given. */
  workspace?: string;
  /** Ask the mentor's `count_tokens` (needs an API key). */
  count?: boolean;
}

export interface PromptBlock {
  text: string;
  /** Carries a cache breakpoint in requests. */
  cache: boolean;
}

export interface PromptShowResult {
  /** `mentor_system_v1`, ... */
  version: string;
  blocks: PromptBlock[];
  /** Set when the blocks came from the session's live conversation. */
  session_id?: string;
  workspace?: string;
  tokens?: number;
  token_error?: string;
}

// ---------------------------------------------------------------- permission.*

export interface PermissionRespondParams {
  request_id: string;
  answer: PermissionAnswer;
  /** The rule to write instead of the first suggested one. */
  rule?: RuleSpec;
}

// ---------------------------------------------------------------- workspace.*

export interface WorkspaceSummary {
  id: string;
  root: string;
  name: string;
  created_at: string;
  last_used_at: string;
}

export interface WorkspaceAddParams {
  root: string;
  name?: string;
}

export interface WorkspaceListResult {
  workspaces: WorkspaceSummary[];
}

export interface WorkspaceIdParams {
  id: string;
}

export interface WorkspaceRemoveResult {
  sessions_unlinked: number;
}

export interface WorkspaceInfoResult extends WorkspaceSummary {
  file_count: number;
  index_truncated?: boolean;
  index_age_s: number;
  git_head?: string;
  git_branch?: string;
  /** Staged, unstaged or untracked changes; absent when not a work tree. */
  git_dirty?: boolean;
  has_instructions: boolean;
  has_config: boolean;
  has_ignore_file: boolean;
  config_overrides?: string[];
}

export interface WorkspaceInitParams {
  id: string;
  /** Replace an existing `HARNESS.md`. */
  force?: boolean;
}

export interface WorkspaceInitResult {
  /** The file written, absolute. */
  path: string;
  replaced?: boolean;
}

/** Every method: wire name → { params, result }. */
export interface Methods {
  "daemon.hello": { params: HelloParams; result: HelloResult };
  "daemon.status": { params: Empty; result: DaemonStatusResult };
  "daemon.shutdown": { params: ShutdownParams; result: Empty };
  "config.get": { params: ConfigGetParams; result: ConfigGetResult };
  "config.set": { params: ConfigSetParams; result: Empty };
  "config.path": { params: Empty; result: ConfigPathResult };
  "auth.set_key": { params: AuthSetKeyParams; result: Empty };
  "auth.status": { params: Empty; result: AuthStatusResult };
  "session.create": { params: SessionCreateParams; result: SessionCreateResult };
  "session.list": { params: SessionListParams; result: SessionListResult };
  "session.get": { params: SessionGetParams; result: SessionGetResult };
  "session.search": { params: SessionSearchParams; result: SessionSearchResult };
  "session.archive": { params: SessionArchiveParams; result: Empty };
  "session.delete": { params: SessionDeleteParams; result: SessionDeleteResult };
  "session.rename": { params: SessionRenameParams; result: Empty };
  "session.export": { params: SessionIdParams; result: SessionExport };
  "session.mark": { params: SessionMarkParams; result: SessionMarkResult };
  "agent.run": { params: AgentRunParams; result: AgentRunResult };
  "agent.cancel": { params: AgentIdParams; result: Empty };
  "agent.subscribe": { params: AgentIdParams; result: AgentSubscribeResult };
  "trace.list": { params: TraceListParams; result: TraceListResult };
  "trace.get": { params: TraceGetParams; result: TraceGetResult };
  "trace.export": { params: TraceExportParams; result: TraceExportResult };
  "trace.import": { params: TraceImportParams; result: TraceImportResult };
  "trace.replay_check": { params: TraceReplayCheckParams; result: ReplayReport };
  "stats.tokens": { params: StatsTokensParams; result: TokenStats };
  "stats.calls": { params: StatsCallsParams; result: StatsCallsResult };
  "stats.reprice": { params: StatsRepriceParams; result: StatsRepriceResult };
  "stats.outcomes": { params: StatsOutcomesParams; result: OutcomeStats };
  "tools.list": { params: ToolsListParams; result: ToolsListResult };
  "tools.rules": { params: ToolsRulesParams; result: ToolsRulesResult };
  "tools.allow": { params: ToolsRuleParams; result: ToolsRuleResult };
  "tools.deny": { params: ToolsRuleParams; result: ToolsRuleResult };
  "tools.remove": { params: ToolsRemoveParams; result: ToolsRemoveResult };
  "prompt.show": { params: PromptShowParams; result: PromptShowResult };
  "permission.respond": { params: PermissionRespondParams; result: Empty };
  "workspace.add": { params: WorkspaceAddParams; result: WorkspaceSummary };
  "workspace.list": { params: Empty; result: WorkspaceListResult };
  "workspace.remove": { params: WorkspaceIdParams; result: WorkspaceRemoveResult };
  "workspace.info": { params: WorkspaceIdParams; result: WorkspaceInfoResult };
  "workspace.refresh": { params: WorkspaceIdParams; result: WorkspaceInfoResult };
  "workspace.init": { params: WorkspaceInitParams; result: WorkspaceInitResult };
}

export type MethodName = keyof Methods;

/** Methods whose result carries a `subscription` (used with `stream`). */
export type StreamingMethod = {
  [M in MethodName]: Methods[M]["result"] extends { subscription: string } ? M : never;
}[MethodName];

export const ALL_METHODS: readonly MethodName[] = [
  "daemon.hello",
  "daemon.status",
  "daemon.shutdown",
  "config.get",
  "config.set",
  "config.path",
  "auth.set_key",
  "auth.status",
  "session.create",
  "session.list",
  "session.get",
  "session.search",
  "session.archive",
  "session.delete",
  "session.rename",
  "session.export",
  "session.mark",
  "agent.run",
  "agent.cancel",
  "agent.subscribe",
  "trace.list",
  "trace.get",
  "trace.export",
  "trace.import",
  "trace.replay_check",
  "stats.tokens",
  "stats.calls",
  "stats.reprice",
  "stats.outcomes",
  "tools.list",
  "tools.rules",
  "tools.allow",
  "tools.deny",
  "tools.remove",
  "prompt.show",
  "permission.respond",
  "workspace.add",
  "workspace.list",
  "workspace.remove",
  "workspace.info",
  "workspace.refresh",
  "workspace.init",
];

// ---------------------------------------------------------------- events

export type AgentStatus = "ok" | "cancelled" | "error";
export type LogLevel = "debug" | "info" | "warn" | "error";
export type Risk = "read_only" | "write" | "execute" | "network";
export type ToolStream = "stdout" | "stderr";
export type StepPhase = "mentor" | "tools";

export type KnownEvent =
  | { type: "agent.started"; agent_id: string; session_id: string }
  | { type: "agent.text_delta"; agent_id: string; text: string }
  | { type: "agent.thinking_delta"; agent_id: string; text: string }
  /** A step of the loop begins a phase; `seq` counts the agent's steps from 1. */
  | { type: "agent.step"; agent_id: string; seq: number; phase: StepPhase }
  | { type: "agent.tool_call"; agent_id: string; call_id: string; name: string; input: unknown }
  /** Partial output of a running tool (the shell streams it). */
  | {
      type: "agent.tool_progress";
      agent_id: string;
      call_id: string;
      stream: ToolStream;
      text: string;
    }
  | {
      type: "agent.tool_result";
      agent_id: string;
      call_id: string;
      name?: string;
      ok: boolean;
      summary: string;
      blob_id?: string;
      /** Length of the result text the mentor receives. */
      mentor_bytes?: number;
      /** The `tool.result` trace event; its blob is the raw output. */
      event_id?: string;
    }
  | {
      type: "agent.usage";
      agent_id: string;
      call_id: string;
      usage: Usage;
      cost_usd?: number;
      /** Running totals of the session, this call included. */
      session_usage?: Usage;
      /** Running cost of the session; absent when a call was unpriced. */
      session_cost_usd?: number;
      /** Mentor calls of the session so far, this one included. */
      session_calls?: number;
    }
  /** `context_large`, `tools_changed`, `stream_interrupted`: worth showing, not fatal. */
  | { type: "agent.warning"; agent_id: string; kind: string; message: string }
  /** The agent waits before calling again (`rate_limited`, `overloaded`); `until` is RFC 3339. */
  | { type: "agent.waiting"; agent_id: string; reason: string; until: string; wait_ms: number }
  /** An `outcome` event was recorded for the run: a test or build result, the files changed, an error, a revert. */
  | {
      type: "agent.outcome";
      agent_id: string;
      event_id: string;
      kind: OutcomeKind | string;
      summary: string;
      ok?: boolean;
      details: unknown;
    }
  | {
      type: "agent.finished";
      agent_id: string;
      status: AgentStatus;
      error?: RpcError;
      /** The model stopped at `max_tokens`: the answer is incomplete. */
      truncated?: boolean;
    }
  | {
      type: "permission.request";
      request_id: string;
      agent_id: string;
      tool: string;
      /** The input as it will run; long strings other than paths and commands cut. */
      input: unknown;
      risk: Risk;
      description?: string;
      command?: string;
      paths?: string[];
      /** Rules an `allow_workspace`/`allow_always`/`deny_always` answer would write. */
      suggested_rules?: RuleSpec[];
      /** Seconds until the request is denied as timed out. */
      timeout_s?: number;
    }
  | {
      type: "permission.decision";
      agent_id: string;
      call_id: string;
      tool: string;
      decision: PermissionDecision;
      source: PermissionSource;
      request_id?: string;
      rule_ref?: string;
      reason?: string;
    }
  | { type: "log"; level: LogLevel; message: string };

export type KnownEventType = KnownEvent["type"];

export const KNOWN_EVENT_TYPES: readonly KnownEventType[] = [
  "agent.started",
  "agent.text_delta",
  "agent.thinking_delta",
  "agent.step",
  "agent.tool_call",
  "agent.tool_progress",
  "agent.tool_result",
  "agent.usage",
  "agent.warning",
  "agent.waiting",
  "agent.outcome",
  "agent.finished",
  "permission.request",
  "permission.decision",
  "log",
];

/** An event type this build does not know; clients must skip it. */
export interface UnknownEvent {
  type: string;
  [k: string]: unknown;
}

export type Event = KnownEvent | UnknownEvent;

/** Narrows to the variants this build knows (`undefined` for the rest). */
export function asKnown(ev: Event): KnownEvent | undefined {
  return (KNOWN_EVENT_TYPES as readonly string[]).includes(ev.type)
    ? (ev as KnownEvent)
    : undefined;
}

export interface EventNotification {
  subscription: string;
  seq: number;
  event: Event;
}

export function isEventNotification(v: unknown): v is EventNotification {
  if (typeof v !== "object" || v === null) return false;
  const o = v as Record<string, unknown>;
  return (
    typeof o.subscription === "string" &&
    typeof o.seq === "number" &&
    typeof o.event === "object" &&
    o.event !== null &&
    typeof (o.event as Record<string, unknown>).type === "string"
  );
}

export function isTerminal(ev: Event): boolean {
  return ev.type === "agent.finished";
}

/**
 * Structural check of one event against the shape this build knows.
 * Unknown types pass (clients must ignore them); known types must carry
 * their fields with the right primitive types.
 */
export function eventShapeError(event: Event): string | undefined {
  const ev = asKnown(event);
  if (ev === undefined) return undefined;
  const o = ev as Record<string, unknown>;
  const str = (k: string) => (typeof o[k] === "string" ? undefined : `${k} must be a string`);
  const bool = (k: string) => (typeof o[k] === "boolean" ? undefined : `${k} must be a boolean`);
  const first = (...checks: (string | undefined)[]) => checks.find((c) => c !== undefined);
  switch (ev.type) {
    case "agent.started":
      return first(str("agent_id"), str("session_id"));
    case "agent.text_delta":
    case "agent.thinking_delta":
      return first(str("agent_id"), str("text"));
    case "agent.step":
      if (typeof o.seq !== "number") return "seq must be a number";
      if (!["mentor", "tools"].includes(String(o.phase))) return "bad phase";
      return str("agent_id");
    case "agent.tool_call":
      return first(
        str("agent_id"),
        str("call_id"),
        str("name"),
        "input" in o ? undefined : "input",
      );
    case "agent.tool_progress":
      if (!["stdout", "stderr"].includes(String(o.stream))) return "bad stream";
      return first(str("agent_id"), str("call_id"), str("text"));
    case "agent.tool_result":
      return first(str("agent_id"), str("call_id"), bool("ok"), str("summary"));
    case "agent.warning":
      return first(str("agent_id"), str("kind"), str("message"));
    case "agent.outcome":
      return first(str("agent_id"), str("event_id"), str("kind"), str("summary"));
    case "agent.waiting":
      if (typeof o.wait_ms !== "number") return "wait_ms must be a number";
      return first(str("agent_id"), str("reason"), str("until"));
    case "agent.usage": {
      const u = o.usage as Record<string, unknown> | undefined;
      if (typeof u !== "object" || u === null) return "usage must be an object";
      for (const k of [
        "input_tokens",
        "output_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
      ]) {
        if (typeof u[k] !== "number") return `usage.${k} must be a number`;
      }
      if (o.cost_usd !== undefined && typeof o.cost_usd !== "number") {
        return "cost_usd must be a number";
      }
      return first(str("agent_id"), str("call_id"));
    }
    case "agent.finished":
      if (!["ok", "cancelled", "error"].includes(String(o.status))) return "bad status";
      if (o.error !== undefined && !isRpcError(o.error)) return "error must be an RpcError";
      if (o.truncated !== undefined && typeof o.truncated !== "boolean") return "bad truncated";
      return str("agent_id");
    case "permission.request":
      return first(
        str("request_id"),
        str("agent_id"),
        str("tool"),
        str("risk"),
        "input" in o ? undefined : "input",
      );
    case "permission.decision":
      if (!["allow", "deny"].includes(String(o.decision))) return "bad decision";
      return first(str("agent_id"), str("call_id"), str("tool"), str("source"));
    case "log":
      return first(str("level"), str("message"));
  }
}
