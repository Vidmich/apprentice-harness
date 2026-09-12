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
];

// ---------------------------------------------------------------- events

export type AgentStatus = "ok" | "cancelled" | "error";
export type LogLevel = "debug" | "info" | "warn" | "error";
export type Risk = "read_only" | "write" | "execute" | "network";

export type KnownEvent =
  | { type: "agent.started"; agent_id: string; session_id: string }
  | { type: "agent.text_delta"; agent_id: string; text: string }
  | { type: "agent.thinking_delta"; agent_id: string; text: string }
  | { type: "agent.tool_call"; agent_id: string; call_id: string; name: string; input: unknown }
  | {
      type: "agent.tool_result";
      agent_id: string;
      call_id: string;
      ok: boolean;
      summary: string;
      blob_id?: string;
    }
  | { type: "agent.usage"; agent_id: string; call_id: string; usage: Usage; cost_usd?: number }
  | { type: "agent.finished"; agent_id: string; status: AgentStatus; error?: RpcError }
  | {
      type: "permission.request";
      request_id: string;
      agent_id: string;
      tool: string;
      input: unknown;
      risk: Risk;
    }
  | { type: "log"; level: LogLevel; message: string };

export type KnownEventType = KnownEvent["type"];

export const KNOWN_EVENT_TYPES: readonly KnownEventType[] = [
  "agent.started",
  "agent.text_delta",
  "agent.thinking_delta",
  "agent.tool_call",
  "agent.tool_result",
  "agent.usage",
  "agent.finished",
  "permission.request",
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
    case "agent.tool_call":
      return first(
        str("agent_id"),
        str("call_id"),
        str("name"),
        "input" in o ? undefined : "input",
      );
    case "agent.tool_result":
      return first(str("agent_id"), str("call_id"), bool("ok"), str("summary"));
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
      return str("agent_id");
    case "permission.request":
      return first(
        str("request_id"),
        str("agent_id"),
        str("tool"),
        str("risk"),
        "input" in o ? undefined : "input",
      );
    case "log":
      return first(str("level"), str("message"));
  }
}
