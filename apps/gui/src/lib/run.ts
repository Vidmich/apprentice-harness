// One `agent.run` as the Playground sees it: a pure reducer over the
// subscription's events plus the usage line, rendered the way the CLI
// renders it. No decisions live here — the daemon makes them; this only
// accumulates what it says.

import {
  type AgentStatus,
  type EventNotification,
  type RpcError,
  type Usage,
  asKnown,
} from "./api";

export type RunPhase = "idle" | "starting" | "running" | "cancelling" | "done";

export interface RunState {
  phase: RunPhase;
  sessionId?: string;
  agentId?: string;
  /** Events are accepted only from this subscription. */
  subscription?: string;
  /** Streamed assistant text. */
  text: string;
  /** Streamed thinking text (shown on request). */
  thinking: string;
  /** Tool activity and log lines, oldest first. */
  activity: string[];
  usage: Usage;
  /** Sum of `cost_usd` over the calls that reported one. */
  costUsd?: number;
  status?: AgentStatus;
  /** Finished `ok` but the model hit `max_tokens`. */
  truncated?: boolean;
  error?: RpcError;
  startedAt?: number;
  finishedAt?: number;
  /** Highest `seq` seen, for debugging dropped events. */
  lastSeq: number;
}

export const ZERO_USAGE: Usage = {
  input_tokens: 0,
  output_tokens: 0,
  cache_read_input_tokens: 0,
  cache_creation_input_tokens: 0,
};

export function idleRun(): RunState {
  return { phase: "idle", text: "", thinking: "", activity: [], usage: ZERO_USAGE, lastSeq: 0 };
}

/** The run was requested; nothing streamed yet. */
export function startingRun(sessionId: string, now: number): RunState {
  return { ...idleRun(), phase: "starting", sessionId, startedAt: now };
}

/**
 * `agent.run` answered. Events may already have arrived on the channel
 * (they adopt the subscription themselves), so a run that finished before
 * the answer stays finished.
 */
export function attachRun(run: RunState, agentId: string, subscription: string): RunState {
  const phase = run.phase === "starting" ? "running" : run.phase;
  return { ...run, phase, agentId, subscription };
}

/** `agent.run` itself failed (no events will come). */
export function failRun(run: RunState, error: RpcError, now: number): RunState {
  return { ...run, phase: "done", status: "error", error, finishedAt: now };
}

export function cancellingRun(run: RunState): RunState {
  return run.phase === "running" ? { ...run, phase: "cancelling" } : run;
}

/**
 * Folds one event into the run. The channel is per run, so the first
 * event tells a `starting` run its subscription; from then on events for
 * another subscription are ignored (two Playground tabs never cross), and
 * so are unknown event types.
 */
export function applyEvent(run: RunState, ev: EventNotification, now: number): RunState {
  if (run.phase === "idle" || run.phase === "done") return run;
  if (run.subscription !== undefined && ev.subscription !== run.subscription) return run;
  const next: RunState = {
    ...run,
    phase: run.phase === "starting" ? "running" : run.phase,
    subscription: run.subscription ?? ev.subscription,
    lastSeq: Math.max(run.lastSeq, ev.seq),
  };
  const e = asKnown(ev.event);
  if (e === undefined) return next;
  switch (e.type) {
    case "agent.started":
      return { ...next, agentId: e.agent_id, sessionId: e.session_id };
    case "agent.text_delta":
      return { ...next, text: next.text + e.text };
    case "agent.thinking_delta":
      return { ...next, thinking: next.thinking + e.text };
    case "agent.tool_call":
      return { ...next, activity: [...next.activity, `→ ${e.name} ${compact(e.input)}`] };
    case "agent.tool_result":
      return {
        ...next,
        activity: [...next.activity, `← ${e.ok ? "ok" : "failed"} ${e.summary}`],
      };
    case "agent.usage": {
      const u = next.usage;
      const usage: Usage = {
        input_tokens: u.input_tokens + e.usage.input_tokens,
        output_tokens: u.output_tokens + e.usage.output_tokens,
        cache_read_input_tokens: u.cache_read_input_tokens + e.usage.cache_read_input_tokens,
        cache_creation_input_tokens:
          u.cache_creation_input_tokens + e.usage.cache_creation_input_tokens,
      };
      const costUsd = e.cost_usd === undefined ? next.costUsd : (next.costUsd ?? 0) + e.cost_usd;
      return costUsd === undefined ? { ...next, usage } : { ...next, usage, costUsd };
    }
    case "agent.finished": {
      const done: RunState = { ...next, phase: "done", status: e.status, finishedAt: now };
      const withError = e.error === undefined ? done : { ...done, error: e.error };
      return e.truncated ? { ...withError, truncated: true } : withError;
    }
    case "permission.request":
      return { ...next, activity: [...next.activity, `? permission: ${e.tool} (${e.risk})`] };
    case "log":
      return { ...next, activity: [...next.activity, `${e.level}: ${e.message}`] };
  }
}

/** One-line JSON cut to 120 characters, as in the CLI's activity log. */
export function compact(v: unknown): string {
  const s = JSON.stringify(v) ?? "null";
  return s.length > 120 ? `${s.slice(0, 117)}...` : s;
}

export function thousands(n: number): string {
  return n.toLocaleString("en-US");
}

export function usd(v: number): string {
  return v !== 0 && Math.abs(v) < 1 ? `$${v.toFixed(4)}` : `$${v.toFixed(2)}`;
}

/** `↳ in 1,204 · out 310 · cache read 0 · $0.0138 · 4.2s` (CLI parity). */
export function usageLine(usage: Usage, costUsd: number | undefined, elapsedS: number): string {
  let line = `↳ in ${thousands(usage.input_tokens)} · out ${thousands(usage.output_tokens)} · cache read ${thousands(usage.cache_read_input_tokens)}`;
  if (usage.cache_creation_input_tokens > 0) {
    line += ` · cache write ${thousands(usage.cache_creation_input_tokens)}`;
  }
  if (costUsd !== undefined) line += ` · ${usd(costUsd)}`;
  line += ` · ${elapsedS.toFixed(1)}s`;
  return line;
}

export function elapsedSeconds(run: RunState, now: number): number {
  if (run.startedAt === undefined) return 0;
  return ((run.finishedAt ?? now) - run.startedAt) / 1000;
}

export function runUsageLine(run: RunState, now: number): string {
  return usageLine(run.usage, run.costUsd, elapsedSeconds(run, now));
}
