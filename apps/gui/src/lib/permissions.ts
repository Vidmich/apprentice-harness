// Permission requests waiting for an answer, routed by session. Pure:
// `lib/chat.ts` folds the events of every subscription through
// `applyPermissionEvent` with the session they belong to; the dialog
// shows `pendingFor` the session in front, the sidebar badges the rest.
// The daemon decides everything else (first answer wins, timeouts): a
// `permission.decision` naming the request closes it here.

import type { Event, Risk, RuleSpec } from "./api";
import { asKnown } from "./api";

export interface PendingPermission {
  requestId: string;
  sessionId: string;
  agentId: string;
  tool: string;
  risk: Risk;
  /** One line: the tool's own description of the call, else the command or paths. */
  description: string;
  command?: string;
  paths: string[];
  /** The input as it will run, long strings cut. */
  input: unknown;
  /** What `allow_workspace` / `allow_always` / `deny_always` would write, most specific first. */
  suggestedRules: RuleSpec[];
  /** Seconds the daemon waits before denying as timed out. */
  timeoutS: number;
  /** When it arrived here (ms), for the countdown. */
  receivedAt: number;
}

export interface PermissionState {
  pending: Record<string, PendingPermission>;
  /** Request ids in arrival order. */
  order: string[];
}

export const EMPTY_PERMISSIONS: PermissionState = { pending: {}, order: [] };

/** Folds one event of `sessionId`'s subscription in. Returns the same state when nothing changed. */
export function applyPermissionEvent(
  state: PermissionState,
  sessionId: string,
  event: Event,
  now: number,
): PermissionState {
  const e = asKnown(event);
  if (e === undefined) return state;
  switch (e.type) {
    case "permission.request": {
      if (state.pending[e.request_id] !== undefined) return state;
      const p: PendingPermission = {
        requestId: e.request_id,
        sessionId,
        agentId: e.agent_id,
        tool: e.tool,
        risk: e.risk,
        description: e.description ?? "",
        paths: e.paths ?? [],
        input: e.input,
        suggestedRules: e.suggested_rules ?? [],
        timeoutS: e.timeout_s ?? 0,
        receivedAt: now,
      };
      if (e.command !== undefined) p.command = e.command;
      return {
        pending: { ...state.pending, [e.request_id]: p },
        order: [...state.order, p.requestId],
      };
    }
    case "permission.decision":
      return e.request_id === undefined ? state : withoutRequest(state, e.request_id);
    case "agent.finished": {
      // Whatever the agent was still asking is moot.
      const gone = state.order.filter((id) => state.pending[id]?.agentId === e.agent_id);
      return gone.reduce(withoutRequest, state);
    }
    default:
      return state;
  }
}

/** The state without `requestId` (answered here, elsewhere, or expired). */
export function withoutRequest(state: PermissionState, requestId: string): PermissionState {
  if (state.pending[requestId] === undefined) return state;
  const pending = { ...state.pending };
  delete pending[requestId];
  return { pending, order: state.order.filter((id) => id !== requestId) };
}

/** Drops requests the daemon has timed out by now (a decision event follows anyway). */
export function expired(state: PermissionState, now: number): PermissionState {
  const gone = state.order.filter((id) => secondsLeft(state.pending[id]!, now) <= 0);
  return gone.reduce(withoutRequest, state);
}

/** The session's open requests, oldest first. */
export function pendingFor(state: PermissionState, sessionId: string): PendingPermission[] {
  return state.order
    .map((id) => state.pending[id])
    .filter((p): p is PendingPermission => p !== undefined && p.sessionId === sessionId);
}

/** Open requests per session. */
export function countBySession(state: PermissionState): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const id of state.order) {
    const p = state.pending[id];
    if (p !== undefined) counts[p.sessionId] = (counts[p.sessionId] ?? 0) + 1;
  }
  return counts;
}

/** Seconds until the daemon denies the request as timed out (never negative). */
export function secondsLeft(p: PendingPermission, now: number): number {
  if (p.timeoutS <= 0) return Number.POSITIVE_INFINITY;
  return Math.max(0, Math.ceil((p.receivedAt + p.timeoutS * 1000 - now) / 1000));
}

/** `9:58` from seconds. */
export function countdown(seconds: number): string {
  if (!Number.isFinite(seconds)) return "";
  const m = Math.floor(seconds / 60);
  return `${m}:${String(seconds - m * 60).padStart(2, "0")}`;
}
