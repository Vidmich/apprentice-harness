// The transcript of one session: the stored messages (`session.get`) plus
// a live overlay for the run in progress, folded from the events of
// `agent.run` / `agent.subscribe`. Pure: every function here returns a
// new value and decides nothing — the daemon already did.
//
// Two sources, one list. The stored rows are the truth and arrive in
// pages (the newest first) and in refreshes at the step boundaries of a
// run; the live overlay holds what is not stored yet — the prompt until
// its row lands, the streaming steps until their assistant message does
// — and shrinks as the rows arrive (`mergeStored`). Facts the rows do
// not carry (a tool's summary, its streamed output, the trace event of
// its result; a run's usage, warnings and error) live beside them, keyed
// by call id and agent id, so a refresh never loses them.

import {
  type AgentStatus,
  type AgentSummary,
  type EventNotification,
  type RpcError,
  type SessionGetResult,
  type SessionInfo,
  type SessionMessage,
  type Usage,
  asKnown,
} from "./api";
import { type ToolResultBlock, blocksOf, resultText } from "./content";
import { ZERO_USAGE, addUsage } from "./format";

export type CallStatus = "running" | "ok" | "error" | "denied";

export interface ProgressChunk {
  stream: "stdout" | "stderr";
  text: string;
}

/** What the events said about one tool call (the rows only carry its input and result text). */
export interface CallMeta {
  callId: string;
  name: string;
  /** From the `agent.tool_call` event (the stored `tool_use` block has it too). */
  input?: unknown;
  status: CallStatus;
  summary?: string;
  blobId?: string;
  /** The `tool.result` trace event; its blob is the raw output. */
  eventId?: string;
  mentorBytes?: number;
  /** Streamed output (the shell), in order. */
  progress: ProgressChunk[];
  progressBytes: number;
  startedAt?: number;
  endedAt?: number;
  permission?: { decision: "allow" | "deny"; source: string; reason?: string };
}

export interface Note {
  kind: "warning" | "waiting" | "permission" | "log";
  text: string;
}

/** One agent (run) of the session: the turn footer's facts. */
export interface AgentTurn {
  id: string;
  status: "running" | AgentStatus;
  model?: string;
  calls: number;
  usage: Usage;
  costUsd?: number;
  startedAt?: number;
  endedAt?: number;
  error?: RpcError;
  truncated?: boolean;
  notes: Note[];
  /** Highest `seq` seen on its subscription, for debugging dropped events. */
  lastSeq: number;
}

/** One mentor call of the live run and what streamed during it. */
export interface LiveStep {
  seq: number;
  thinking: string;
  text: string;
  /** Tool calls announced in this step, in order. */
  calls: string[];
}

export type LivePhase = "starting" | "running" | "cancelling" | "done";

export interface LiveRun {
  phase: LivePhase;
  agentId?: string;
  /** Events are accepted only from this subscription once known. */
  subscription?: string;
  /** The prompt as typed, until its row is stored. */
  prompt?: string;
  /** Steps whose assistant message is not stored yet. */
  steps: LiveStep[];
  startedAt: number;
  /** `agent.run` itself failed: no agent, no events. */
  error?: RpcError;
}

export interface Transcript {
  sessionId: string;
  info?: SessionInfo;
  /** Stored rows, ascending and contiguous from the oldest loaded. */
  messages: SessionMessage[];
  hasOlder: boolean;
  calls: Record<string, CallMeta>;
  agents: Record<string, AgentTurn>;
  live?: LiveRun;
  loading: boolean;
  loadError?: string;
}

export function emptyTranscript(sessionId: string): Transcript {
  return {
    sessionId,
    messages: [],
    hasOlder: false,
    calls: {},
    agents: {},
    loading: false,
  };
}

/** A run is in flight (events may still arrive). */
export function isBusy(t: Transcript): boolean {
  const phase = t.live?.phase;
  return phase === "starting" || phase === "running" || phase === "cancelling";
}

export function maxSeq(t: Transcript): number {
  const last = t.messages[t.messages.length - 1];
  return last === undefined ? 0 : last.seq;
}

// ---------------------------------------------------------------- stored rows

function parseTime(ts: string | undefined): number | undefined {
  if (ts === undefined) return undefined;
  const ms = Date.parse(ts);
  return Number.isNaN(ms) ? undefined : ms;
}

function turnFromSummary(a: AgentSummary, previous: AgentTurn | undefined): AgentTurn {
  const base: AgentTurn = previous ?? {
    id: a.id,
    status: a.status,
    calls: 0,
    usage: ZERO_USAGE,
    notes: [],
    lastSeq: 0,
  };
  const turn: AgentTurn = {
    ...base,
    // The events know a finished run before the row does; never go back.
    status: base.status !== "running" && a.status === "running" ? base.status : a.status,
    calls: Math.max(base.calls, a.calls),
    usage: a.calls >= base.calls ? a.usage : base.usage,
  };
  if (a.model !== undefined) turn.model = a.model;
  if (a.cost_usd !== undefined && a.calls >= base.calls) turn.costUsd = a.cost_usd;
  const started = parseTime(a.started_at);
  if (started !== undefined) turn.startedAt = started;
  const ended = parseTime(a.ended_at);
  if (ended !== undefined) turn.endedAt = ended;
  return turn;
}

/**
 * Folds a `session.get` page in. `newest` and `older` pages set the
 * paging state; a `refresh` (rows at and after the last known one)
 * keeps it. Rows with a known `seq` replace theirs (a user turn that
 * grew), new ones are inserted in order. The live overlay drops the
 * prompt once its row is stored and one step per newly stored assistant
 * message of the live agent.
 */
export function mergeStored(
  t: Transcript,
  page: SessionGetResult,
  mode: "newest" | "older" | "refresh",
): Transcript {
  const before = maxSeq(t);
  const bySeq = new Map(t.messages.map((m) => [m.seq, m]));
  for (const m of page.messages) bySeq.set(m.seq, m);
  const messages = [...bySeq.values()].sort((a, b) => a.seq - b.seq);
  const agents = { ...t.agents };
  for (const a of page.agents ?? []) agents[a.id] = turnFromSummary(a, agents[a.id]);
  const next: Transcript = {
    ...t,
    info: page.session,
    messages,
    hasOlder: mode === "refresh" ? t.hasOlder : page.has_more === true,
    agents,
    loading: false,
  };
  delete next.loadError;
  if (t.live === undefined) return next;
  const live = { ...t.live };
  if (live.agentId !== undefined) {
    const mine = page.messages.filter((m) => m.agent_id === live.agentId);
    if (live.prompt !== undefined && mine.some((m) => m.role === "user")) delete live.prompt;
    const storedSteps = mine.filter((m) => m.role === "assistant" && m.seq > before).length;
    if (storedSteps > 0) live.steps = live.steps.slice(storedSteps);
  }
  return { ...next, live };
}

export function loadingTranscript(t: Transcript): Transcript {
  const next = { ...t, loading: true };
  delete next.loadError;
  return next;
}

export function failedLoad(t: Transcript, error: string): Transcript {
  return { ...t, loading: false, loadError: error };
}

// ---------------------------------------------------------------- the live run

/** The prompt was sent; nothing answered yet. */
export function startLive(t: Transcript, prompt: string, now: number): Transcript {
  return { ...t, live: { phase: "starting", prompt, steps: [], startedAt: now } };
}

/** `agent.run` (or `agent.subscribe`) answered. */
export function attachLive(
  t: Transcript,
  agentId: string,
  subscription: string | undefined,
  now: number,
): Transcript {
  const live: LiveRun = t.live ?? { phase: "starting", steps: [], startedAt: now };
  const phase = live.phase === "starting" ? "running" : live.phase;
  const agents = { ...t.agents };
  agents[agentId] ??= {
    id: agentId,
    status: "running",
    calls: 0,
    usage: ZERO_USAGE,
    notes: [],
    lastSeq: 0,
    startedAt: now,
  };
  const next: LiveRun = { ...live, phase, agentId };
  if (subscription !== undefined) next.subscription = subscription;
  return { ...t, agents, live: next };
}

/** Remembers the `tool.result` trace event of a call (found in the trace). */
export function setCallEvent(t: Transcript, callId: string, eventId: string): Transcript {
  if (t.calls[callId]?.eventId === eventId) return t;
  return { ...t, calls: patchCall(t, callId, { eventId }) };
}

/** `agent.run` itself failed (no events will come). */
export function failLive(t: Transcript, error: RpcError): Transcript {
  const live: LiveRun = t.live ?? { phase: "done", steps: [], startedAt: 0 };
  return { ...t, live: { ...live, phase: "done", error } };
}

export function cancellingLive(t: Transcript): Transcript {
  if (t.live === undefined || t.live.phase !== "running") return t;
  return { ...t, live: { ...t.live, phase: "cancelling" } };
}

/** Forgets a finished overlay with nothing left to show. */
export function clearLive(t: Transcript): Transcript {
  if (t.live === undefined) return t;
  const { live } = t;
  if (live.phase !== "done" || live.error !== undefined) return t;
  if (live.prompt !== undefined || live.steps.length > 0) return t;
  const next = { ...t };
  delete next.live;
  return next;
}

function currentStep(live: LiveRun): [LiveRun, LiveStep] {
  const last = live.steps[live.steps.length - 1];
  if (last !== undefined) return [live, last];
  const step: LiveStep = { seq: 1, thinking: "", text: "", calls: [] };
  return [{ ...live, steps: [step] }, step];
}

function patchStep(live: LiveRun, step: LiveStep, patch: Partial<LiveStep>): LiveRun {
  return { ...live, steps: live.steps.map((s) => (s === step ? { ...s, ...patch } : s)) };
}

function patchAgent(
  t: Transcript,
  agentId: string,
  patch: Partial<AgentTurn> | ((turn: AgentTurn) => AgentTurn),
): Record<string, AgentTurn> {
  const turn: AgentTurn = t.agents[agentId] ?? {
    id: agentId,
    status: "running",
    calls: 0,
    usage: ZERO_USAGE,
    notes: [],
    lastSeq: 0,
  };
  const next = typeof patch === "function" ? patch(turn) : { ...turn, ...patch };
  return { ...t.agents, [agentId]: next };
}

function patchCall(
  t: Transcript,
  callId: string,
  patch: Partial<CallMeta> | ((meta: CallMeta) => CallMeta),
): Record<string, CallMeta> {
  const meta: CallMeta = t.calls[callId] ?? {
    callId,
    name: "",
    status: "running",
    progress: [],
    progressBytes: 0,
  };
  const next = typeof patch === "function" ? patch(meta) : { ...meta, ...patch };
  return { ...t.calls, [callId]: next };
}

function note(t: Transcript, agentId: string, n: Note): Record<string, AgentTurn> {
  return patchAgent(t, agentId, (turn) => ({ ...turn, notes: [...turn.notes, n] }));
}

/**
 * Folds one event in. The first event tells a `starting` run its
 * subscription; from then on events of another subscription are
 * ignored (two chats never cross), and so are unknown types. Events
 * after the terminal one change nothing.
 */
export function applyEvent(t: Transcript, ev: EventNotification, now: number): Transcript {
  if (t.live === undefined || t.live.phase === "done") return t;
  if (t.live.subscription !== undefined && ev.subscription !== t.live.subscription) return t;
  let live: LiveRun = {
    ...t.live,
    phase: t.live.phase === "starting" ? "running" : t.live.phase,
    subscription: t.live.subscription ?? ev.subscription,
  };
  const e = asKnown(ev.event);
  if (e === undefined) return { ...t, live };
  const agentId = "agent_id" in e ? e.agent_id : live.agentId;
  if (agentId !== undefined && live.agentId === undefined) live.agentId = agentId;
  let next: Transcript = { ...t, live };
  if (agentId !== undefined) {
    next.agents = patchAgent(next, agentId, (turn) => ({
      ...turn,
      lastSeq: Math.max(turn.lastSeq, ev.seq),
      startedAt: turn.startedAt ?? now,
    }));
  }
  switch (e.type) {
    case "agent.started":
      return next;
    case "agent.text_delta": {
      const [l, step] = currentStep(live);
      return { ...next, live: patchStep(l, step, { text: step.text + e.text }) };
    }
    case "agent.thinking_delta": {
      const [l, step] = currentStep(live);
      return { ...next, live: patchStep(l, step, { thinking: step.thinking + e.text }) };
    }
    case "agent.step": {
      if (e.phase !== "mentor") return next;
      // A new step; the previous one's calls are over.
      const last = live.steps[live.steps.length - 1];
      if (last !== undefined && last.seq === e.seq) return next;
      const step: LiveStep = { seq: e.seq, thinking: "", text: "", calls: [] };
      return { ...next, live: { ...live, steps: [...live.steps, step] } };
    }
    case "agent.tool_call": {
      const [l, step] = currentStep(live);
      live = patchStep(l, step, { calls: [...step.calls, e.call_id] });
      const calls = patchCall(next, e.call_id, {
        name: e.name,
        input: e.input,
        status: "running",
        startedAt: now,
      });
      return { ...next, live, calls };
    }
    case "agent.tool_progress": {
      const calls = patchCall(next, e.call_id, (meta) => ({
        ...meta,
        progress: [...meta.progress, { stream: e.stream, text: e.text }],
        progressBytes: meta.progressBytes + e.text.length,
      }));
      return { ...next, calls };
    }
    case "agent.tool_result": {
      const calls = patchCall(next, e.call_id, (meta) => {
        const denied = meta.permission?.decision === "deny";
        const done: CallMeta = {
          ...meta,
          name: e.name ?? meta.name,
          status: e.ok ? "ok" : denied ? "denied" : "error",
          summary: e.summary,
          endedAt: now,
        };
        if (e.blob_id !== undefined) done.blobId = e.blob_id;
        if (e.event_id !== undefined) done.eventId = e.event_id;
        if (e.mentor_bytes !== undefined) done.mentorBytes = e.mentor_bytes;
        return done;
      });
      return { ...next, calls };
    }
    case "agent.usage": {
      const agents = patchAgent(next, e.agent_id, (turn) => {
        const usage = addUsage(turn.usage, e.usage);
        const out: AgentTurn = { ...turn, usage, calls: turn.calls + 1 };
        if (e.cost_usd !== undefined) out.costUsd = (turn.costUsd ?? 0) + e.cost_usd;
        return out;
      });
      if (next.info !== undefined && e.session_usage !== undefined) {
        const info: SessionInfo = { ...next.info, usage: e.session_usage };
        if (e.session_cost_usd !== undefined) info.cost_usd = e.session_cost_usd;
        else delete info.cost_usd;
        info.calls = e.session_calls ?? next.info.calls + 1;
        next = { ...next, info };
      }
      return { ...next, agents };
    }
    case "agent.warning":
      return {
        ...next,
        agents: note(next, e.agent_id, { kind: "warning", text: `${e.kind}: ${e.message}` }),
      };
    case "agent.waiting":
      return {
        ...next,
        agents: note(next, e.agent_id, {
          kind: "waiting",
          text: `waiting ${Math.ceil(e.wait_ms / 1000)} s for the mentor (${e.reason})`,
        }),
      };
    case "agent.finished": {
      const agents = patchAgent(next, e.agent_id, (turn) => {
        const done: AgentTurn = { ...turn, status: e.status, endedAt: now };
        if (e.error !== undefined) done.error = e.error;
        if (e.truncated === true) done.truncated = true;
        return done;
      });
      // Calls still running when the agent ended (cancelled under one).
      const calls = { ...next.calls };
      for (const [id, meta] of Object.entries(calls)) {
        if (meta.status === "running") calls[id] = { ...meta, status: "error", endedAt: now };
      }
      return { ...next, agents, calls, live: { ...live, phase: "done" } };
    }
    case "permission.request":
      return {
        ...next,
        agents: note(next, e.agent_id, {
          kind: "permission",
          text: `permission requested: ${e.tool} (${e.risk})`,
        }),
      };
    case "permission.decision": {
      if (e.source === "rule" && e.decision === "allow") return next;
      const permission: CallMeta["permission"] = { decision: e.decision, source: e.source };
      if (e.reason !== undefined) permission.reason = e.reason;
      const calls = patchCall(next, e.call_id, (meta) => ({
        ...meta,
        name: meta.name === "" ? e.tool : meta.name,
        permission,
        status: e.decision === "deny" ? "denied" : meta.status,
      }));
      return { ...next, calls };
    }
    case "log":
      return agentId === undefined
        ? next
        : {
            ...next,
            agents: note(next, agentId, { kind: "log", text: `${e.level}: ${e.message}` }),
          };
  }
}

// ---------------------------------------------------------------- items

export type Item =
  | {
      kind: "user";
      key: string;
      text: string;
      agentId?: string;
      /** Not stored yet. */
      pending?: boolean;
      /** Written by the daemon, not the user. */
      synthetic?: boolean;
    }
  | { kind: "assistant_text"; key: string; text: string; agentId?: string; streaming: boolean }
  | {
      kind: "thinking";
      key: string;
      text: string;
      agentId?: string;
      streaming: boolean;
      redacted?: boolean;
    }
  | {
      kind: "tool_call";
      key: string;
      callId: string;
      name: string;
      input: unknown;
      agentId?: string;
      result?: { text: string; isError: boolean };
      meta?: CallMeta;
      /** No result yet and the run goes on (the results row comes after the step's tools). */
      live: boolean;
    }
  | { kind: "turn_footer"; key: string; agentId: string; turn: AgentTurn; toolCalls: number }
  | { kind: "error"; key: string; error: RpcError; retry?: string };

function toolItem(
  t: Transcript,
  callId: string,
  name: string,
  input: unknown,
  agentId: string | undefined,
  results: Map<string, ToolResultBlock>,
  live: boolean,
): Item {
  const r = results.get(callId);
  const item: Item = {
    kind: "tool_call",
    key: `call:${callId}`,
    callId,
    name,
    input,
    live: live && r === undefined,
  };
  if (agentId !== undefined) item.agentId = agentId;
  const meta = t.calls[callId];
  if (meta !== undefined) item.meta = meta;
  if (r !== undefined) item.result = { text: resultText(r), isError: r.is_error === true };
  return item;
}

/**
 * The transcript as a flat list: the stored rows, then the live overlay,
 * with a footer after the last item of each agent. Text blocks of a user
 * row are one item each (a turn that grew was several prompts); tool
 * results fold into their call's card.
 */
export function deriveItems(t: Transcript): Item[] {
  const results = new Map<string, ToolResultBlock>();
  const parsed = t.messages.map((m) => ({ m, blocks: blocksOf(m.content) }));
  for (const { blocks } of parsed) {
    for (const b of blocks) if (b.type === "tool_result") results.set(b.tool_use_id, b);
  }
  const items: Item[] = [];
  for (const { m, blocks } of parsed) {
    const agentId = m.agent_id ?? undefined;
    blocks.forEach((b, i) => {
      const key = `m${m.seq}:${i}`;
      switch (b.type) {
        case "text":
          if (m.role === "user") {
            const item: Item = { kind: "user", key, text: b.text };
            if (agentId !== undefined) item.agentId = agentId;
            if (isSynthetic(b.text)) item.synthetic = true;
            items.push(item);
          } else {
            const item: Item = { kind: "assistant_text", key, text: b.text, streaming: false };
            if (agentId !== undefined) item.agentId = agentId;
            items.push(item);
          }
          break;
        case "thinking": {
          const item: Item = { kind: "thinking", key, text: b.thinking, streaming: false };
          if (agentId !== undefined) item.agentId = agentId;
          items.push(item);
          break;
        }
        case "redacted_thinking": {
          const item: Item = { kind: "thinking", key, text: "", streaming: false, redacted: true };
          if (agentId !== undefined) item.agentId = agentId;
          items.push(item);
          break;
        }
        case "tool_use":
          // A stored call of the running agent without its result yet is
          // still running (the results row comes after the step's tools).
          items.push(
            toolItem(
              t,
              b.id,
              b.name,
              b.input,
              agentId,
              results,
              agentId !== undefined && agentId === t.live?.agentId && t.live.phase !== "done",
            ),
          );
          break;
        default:
          break;
      }
    });
  }
  const { live } = t;
  if (live !== undefined) {
    const agentId = live.agentId;
    if (live.prompt !== undefined) {
      const item: Item = { kind: "user", key: "live:prompt", text: live.prompt, pending: true };
      if (agentId !== undefined) item.agentId = agentId;
      items.push(item);
    }
    const streaming = live.phase === "running" || live.phase === "cancelling";
    live.steps.forEach((step, i) => {
      const last = i === live.steps.length - 1;
      if (step.thinking !== "") {
        const item: Item = {
          kind: "thinking",
          key: `live:${step.seq}:thinking`,
          text: step.thinking,
          streaming: streaming && last && step.text === "" && step.calls.length === 0,
        };
        if (agentId !== undefined) item.agentId = agentId;
        items.push(item);
      }
      if (step.text !== "") {
        const item: Item = {
          kind: "assistant_text",
          key: `live:${step.seq}:text`,
          text: step.text,
          streaming: streaming && last && step.calls.length === 0,
        };
        if (agentId !== undefined) item.agentId = agentId;
        items.push(item);
      }
      for (const callId of step.calls) {
        const meta = t.calls[callId];
        items.push(toolItem(t, callId, meta?.name ?? "?", meta?.input, agentId, results, true));
      }
    });
    if (live.error !== undefined) {
      const item: Item = { kind: "error", key: "live:error", error: live.error };
      if (live.prompt !== undefined) item.retry = live.prompt;
      items.push(item);
    }
  }
  return withFooters(items, t.agents);
}

/** Inserts each agent's footer after its last item. */
function withFooters(items: Item[], agents: Record<string, AgentTurn>): Item[] {
  const lastIndex = new Map<string, number>();
  const toolCalls = new Map<string, number>();
  items.forEach((item, i) => {
    if ("agentId" in item && item.agentId !== undefined) {
      lastIndex.set(item.agentId, i);
      if (item.kind === "tool_call") {
        toolCalls.set(item.agentId, (toolCalls.get(item.agentId) ?? 0) + 1);
      }
    }
  });
  const out: Item[] = [];
  items.forEach((item, i) => {
    out.push(item);
    if ("agentId" in item && item.agentId !== undefined && lastIndex.get(item.agentId) === i) {
      const turn = agents[item.agentId];
      if (turn !== undefined) {
        out.push({
          kind: "turn_footer",
          key: `footer:${item.agentId}`,
          agentId: item.agentId,
          turn,
          toolCalls: toolCalls.get(item.agentId) ?? 0,
        });
      }
    }
  });
  return out;
}

/** A user text the daemon wrote (`[continue ...]`, `[note: ...]`), not the user. */
export function isSynthetic(text: string): boolean {
  return /^\[(continue|note:)/.test(text);
}

/**
 * The prompt an agent ran on, for Retry: the last real text of the user
 * row stored at its start (a turn that grew ends with the newest prompt).
 */
export function promptOf(t: Transcript, agentId: string): string | undefined {
  let prompt: string | undefined;
  for (const m of t.messages) {
    if (m.agent_id !== agentId || m.role !== "user" || m.step_id !== undefined) continue;
    for (const b of blocksOf(m.content)) {
      if (b.type === "text" && !isSynthetic(b.text)) prompt = b.text;
    }
  }
  return prompt ?? (t.live?.agentId === agentId ? t.live.prompt : undefined);
}
