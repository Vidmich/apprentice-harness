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

export interface SessionSummary {
  id: string;
  title?: string | null;
  workspace?: string | null;
  created_at: string;
  updated_at: string;
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

export interface TokenStats {
  range: StatsRange;
  tz: string;
  totals: TokenBucket;
  by_model: TokenBucket[];
  by_day: TokenBucket[];
  by_session: TokenBucket[];
  apprentice: ApprenticeStats;
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
  limit?: number;
  offset?: number;
}

export interface SessionListResult {
  sessions: SessionSummary[];
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

export interface StatsTokensParams {
  since?: string;
  until?: string;
  session_id?: string;
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
  has_instructions: boolean;
  has_config: boolean;
  has_ignore_file: boolean;
  config_overrides?: string[];
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
  "agent.run": { params: AgentRunParams; result: AgentRunResult };
  "agent.cancel": { params: AgentIdParams; result: Empty };
  "agent.subscribe": { params: AgentIdParams; result: AgentSubscribeResult };
  "trace.list": { params: TraceListParams; result: TraceListResult };
  "trace.get": { params: TraceGetParams; result: TraceGetResult };
  "stats.tokens": { params: StatsTokensParams; result: TokenStats };
  "stats.reprice": { params: StatsRepriceParams; result: StatsRepriceResult };
  "tools.list": { params: ToolsListParams; result: ToolsListResult };
  "tools.rules": { params: ToolsRulesParams; result: ToolsRulesResult };
  "tools.allow": { params: ToolsRuleParams; result: ToolsRuleResult };
  "tools.deny": { params: ToolsRuleParams; result: ToolsRuleResult };
  "prompt.show": { params: PromptShowParams; result: PromptShowResult };
  "permission.respond": { params: PermissionRespondParams; result: Empty };
  "workspace.add": { params: WorkspaceAddParams; result: WorkspaceSummary };
  "workspace.list": { params: Empty; result: WorkspaceListResult };
  "workspace.remove": { params: WorkspaceIdParams; result: WorkspaceRemoveResult };
  "workspace.info": { params: WorkspaceIdParams; result: WorkspaceInfoResult };
  "workspace.refresh": { params: WorkspaceIdParams; result: WorkspaceInfoResult };
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
  "agent.run",
  "agent.cancel",
  "agent.subscribe",
  "trace.list",
  "trace.get",
  "stats.tokens",
  "stats.reprice",
  "tools.list",
  "tools.rules",
  "tools.allow",
  "tools.deny",
  "prompt.show",
  "permission.respond",
  "workspace.add",
  "workspace.list",
  "workspace.remove",
  "workspace.info",
  "workspace.refresh",
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
    }
  /** `context_large`, `tools_changed`, `stream_interrupted`: worth showing, not fatal. */
  | { type: "agent.warning"; agent_id: string; kind: string; message: string }
  /** The agent waits before calling again (`rate_limited`, `overloaded`); `until` is RFC 3339. */
  | { type: "agent.waiting"; agent_id: string; reason: string; until: string; wait_ms: number }
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
