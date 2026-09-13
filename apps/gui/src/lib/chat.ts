// The effects behind the chat view: loading a session's transcript
// (`session.get`, newest page first), sending a prompt (`agent.run`,
// events folded into the transcript), cancelling, reattaching to a run
// after a reload (`agent.subscribe`), and fetching a tool's raw output
// from the trace. Each session has one listener; events are folded in
// batches so a fast stream costs one render per frame, not per delta.
// Permission requests on any listener go to the permissions store,
// keyed by session, and are answered with `respondPermission`.

import { chatOf, chatOfSession, useStore } from "../store";
import { usePermissions } from "../stores/permissions";
import { transcriptOf, useTranscripts } from "../stores/transcripts";
import {
  type EventNotification,
  type PermissionAnswer,
  type RpcError,
  type RuleSpec,
  type SessionGetResult,
  asKnown,
} from "./api";
import { type Unsubscribe, newChannel, subscribe } from "./events";
import { RpcFailure, call, describe, stream } from "./rpc";
import { refreshSessions, refreshWorkspaces } from "./sessions";
import {
  type Transcript,
  applyEvent,
  attachLive,
  cancellingLive,
  clearLive,
  failLive,
  failedLoad,
  isBusy,
  loadingTranscript,
  maxSeq,
  mergeStored,
  setCallEvent,
  startLive,
} from "./transcript";

/** Messages per `session.get` page. */
export const PAGE = 100;
/** Milliseconds after a run ends before the title is read again (the cheap model names the session). */
const TITLE_DELAY_MS = 5000;

const listeners = new Map<string, Unsubscribe>();

function stopListening(sessionId: string): void {
  listeners.get(sessionId)?.();
  listeners.delete(sessionId);
}

function update(sessionId: string, f: (t: Transcript) => Transcript): void {
  useTranscripts.getState().update(sessionId, f);
}

function toError(e: unknown): RpcError {
  return e instanceof RpcFailure ? e.error : { code: -32603, message: String(e) };
}

// ---------------------------------------------------------------- loading

/**
 * Shows a session: loads its newest page unless it is in memory, and
 * reattaches to an agent still running on it.
 */
export async function openSession(sessionId: string): Promise<void> {
  useTranscripts.getState().touch(sessionId);
  const t = transcriptOf(sessionId);
  if (t !== undefined && (t.info !== undefined || t.loading)) return;
  await reloadSession(sessionId);
}

/** Loads the newest page afresh (the overlay of a live run is kept). */
export async function reloadSession(sessionId: string): Promise<void> {
  update(sessionId, loadingTranscript);
  let page: SessionGetResult;
  try {
    page = await call("session.get", {
      id: sessionId,
      before_seq: Number.MAX_SAFE_INTEGER,
      limit: PAGE,
    });
  } catch (e) {
    // A session the daemon no longer has (deleted elsewhere, another
    // data dir): its chat closes rather than showing an error forever.
    if (e instanceof RpcFailure && e.kind === "not_found") {
      const chat = chatOfSession(sessionId);
      forget(sessionId);
      if (chat !== undefined) useStore.getState().closeChat(chat.id);
      return;
    }
    update(sessionId, (t) => failedLoad(t, describe(toError(e))));
    return;
  }
  update(sessionId, (t) => mergeStored(t, page, "newest"));
  const running = (page.agents ?? []).find((a) => a.status === "running");
  const current = transcriptOf(sessionId);
  if (running !== undefined && (current === undefined || !isBusy(current))) {
    await reattach(sessionId, running.id);
  }
}

/** Loads the page before the oldest loaded message. */
export async function loadOlder(sessionId: string): Promise<void> {
  const t = transcriptOf(sessionId);
  const first = t?.messages[0];
  if (t === undefined || t.loading || !t.hasOlder || first === undefined) return;
  update(sessionId, loadingTranscript);
  try {
    const page = await call("session.get", { id: sessionId, before_seq: first.seq, limit: PAGE });
    update(sessionId, (t) => mergeStored(t, page, "older"));
  } catch (e) {
    update(sessionId, (t) => failedLoad(t, describe(toError(e))));
  }
}

/**
 * Reads the rows stored since the last known one (that one included:
 * a user turn that grew is rewritten in place) and the agents.
 */
export async function refresh(sessionId: string): Promise<void> {
  const t = transcriptOf(sessionId);
  if (t === undefined) return;
  let after = Math.max(0, maxSeq(t) - 1);
  for (;;) {
    let page: SessionGetResult;
    try {
      page = await call("session.get", { id: sessionId, after_seq: after, limit: PAGE });
    } catch (e) {
      console.warn("session.get failed:", describe(toError(e)));
      return;
    }
    update(sessionId, (t) => mergeStored(t, page, "refresh"));
    const last = page.messages[page.messages.length - 1];
    if (page.has_more !== true || last === undefined) return;
    after = last.seq;
  }
}

// ---------------------------------------------------------------- events

const queued = new Map<string, EventNotification[]>();
let flushScheduled = false;

/** Events are folded in batches: one render per frame, not per delta. */
const FLUSH_MS = 16;

function queueEvent(sessionId: string, ev: EventNotification): void {
  const q = queued.get(sessionId);
  if (q === undefined) queued.set(sessionId, [ev]);
  else q.push(ev);
  if (!flushScheduled) {
    flushScheduled = true;
    setTimeout(flush, FLUSH_MS);
  }
}

/** Applies every queued event, one store update per session. */
function flush(): void {
  flushScheduled = false;
  const batches = [...queued.entries()];
  queued.clear();
  const now = Date.now();
  const permissions = usePermissions.getState();
  for (const [sessionId, events] of batches) {
    let stepped = false;
    let finished = false;
    for (const { event } of events) {
      const e = asKnown(event);
      if (e?.type === "agent.step" && e.phase === "mentor" && e.seq > 1) stepped = true;
      if (e?.type === "agent.finished") finished = true;
      if (e?.type.startsWith("permission.") === true || finished) {
        permissions.apply(sessionId, event, now);
      }
    }
    // One reducer call per event; the store sees the batch as one change.
    update(sessionId, (t) => events.reduce((next, ev) => applyEvent(next, ev, now), t));
    if (finished) {
      stopListening(sessionId);
      void finish(sessionId);
    } else if (stepped) {
      // A new mentor call: the previous step's rows are stored.
      void refresh(sessionId);
    }
  }
}

async function finish(sessionId: string): Promise<void> {
  await refresh(sessionId);
  update(sessionId, clearLive);
  // The run may have changed the tree (the dirty marker) and the row.
  void refreshWorkspaces().then(refreshSessions);
  const t = transcriptOf(sessionId);
  if (t?.info?.title_source !== "user") {
    setTimeout(() => {
      void refreshInfo(sessionId).then(refreshSessions);
    }, TITLE_DELAY_MS);
  }
}

/** Re-reads the session's info only (title, totals). */
export async function refreshInfo(sessionId: string): Promise<void> {
  try {
    const page = await call("session.get", {
      id: sessionId,
      after_seq: Number.MAX_SAFE_INTEGER,
      limit: 1,
    });
    update(sessionId, (t) => (t.info === undefined ? t : { ...t, info: page.session }));
  } catch (e) {
    console.warn("session.get failed:", describe(toError(e)));
  }
}

// ---------------------------------------------------------------- running

/**
 * Sends a chat's prompt. Creates the session on the first send (the
 * daemon names it after the prompt), then `agent.run` with the events
 * on a channel subscribed before the call so none can slip past.
 */
export async function send(chatId: number, prompt: string): Promise<void> {
  const chat = chatOf(chatId);
  const store = useStore.getState();
  if (chat === undefined || prompt.trim() === "") return;
  let sessionId = chat.sessionId;
  if (sessionId === undefined) {
    const workspace = chat.workspace.trim();
    if (workspace === "") return;
    try {
      sessionId = (await call("session.create", { workspace })).session_id;
    } catch (e) {
      store.updateChat(chatId, { error: describe(toError(e)) });
      return;
    }
    store.updateChat(chatId, { sessionId });
    useTranscripts.getState().touch(sessionId);
    // The daemon registered the folder as a workspace if it was new.
    void refreshWorkspaces().then(refreshSessions);
  }
  const id = sessionId;
  const t = transcriptOf(id);
  if (t !== undefined && isBusy(t)) return;
  stopListening(id);
  const now = Date.now();
  update(id, (t) => startLive(clearLive(t), prompt, now));

  const channel = newChannel();
  const stop = await subscribe(channel, (ev) => queueEvent(id, ev));
  listeners.set(id, stop);
  try {
    const started = await stream("agent.run", { session_id: id, prompt }, channel);
    update(id, (t) => attachLive(t, started.result.agent_id, started.subscription, Date.now()));
    // The user row is stored before `agent.run` answers.
    void refresh(id);
  } catch (e) {
    stopListening(id);
    update(id, (t) => failLive(t, toError(e)));
  }
}

/** Asks the daemon to cancel; the run finishes with `cancelled` via events. */
export async function cancel(sessionId: string): Promise<void> {
  const t = transcriptOf(sessionId);
  const agentId = t?.live?.agentId;
  if (t === undefined || agentId === undefined || t.live?.phase !== "running") return;
  update(sessionId, cancellingLive);
  try {
    await call("agent.cancel", { agent_id: agentId });
  } catch (e) {
    console.warn("agent.cancel failed:", describe(toError(e)));
    update(sessionId, (t) =>
      t.live?.phase === "cancelling" ? { ...t, live: { ...t.live, phase: "running" } } : t,
    );
  }
}

/**
 * Follows an agent already running (after a reload). Past deltas are
 * not replayed: the stored rows fill the gap at the next refresh.
 */
export async function reattach(sessionId: string, agentId: string): Promise<void> {
  stopListening(sessionId);
  const now = Date.now();
  update(sessionId, (t) => attachLive(clearLive(t), agentId, undefined, now));
  const channel = newChannel();
  const stop = await subscribe(channel, (ev) => queueEvent(sessionId, ev));
  listeners.set(sessionId, stop);
  try {
    const started = await stream("agent.subscribe", { agent_id: agentId }, channel);
    update(sessionId, (t) => attachLive(t, agentId, started.subscription, now));
    // Not running: the terminal event follows on the channel.
  } catch (e) {
    stopListening(sessionId);
    update(sessionId, (t) => failLive(t, toError(e)));
  }
}

/**
 * Answers a permission request. The daemon's decision event closes it
 * everywhere; one it no longer knows (answered elsewhere, timed out)
 * just goes.
 */
export async function respondPermission(
  requestId: string,
  answer: PermissionAnswer,
  rule?: RuleSpec,
): Promise<void> {
  const params =
    rule === undefined
      ? { request_id: requestId, answer }
      : { request_id: requestId, answer, rule };
  try {
    await call("permission.respond", params);
  } catch (e) {
    const failure = e instanceof RpcFailure ? e : undefined;
    if (failure?.kind !== "not_found") throw e;
  } finally {
    usePermissions.getState().remove(requestId);
  }
}

/** Drops a session's listener (a closed chat). A running agent keeps running. */
export function forget(sessionId: string): void {
  stopListening(sessionId);
  queued.delete(sessionId);
  useTranscripts.getState().drop(sessionId);
}

// ---------------------------------------------------------------- raw output

export interface RawOutput {
  eventId: string;
  /** The blob as text (`undefined` when the call produced none). */
  text?: string;
  bytes?: number;
  mediaType?: string;
  /** The capture itself was cut (the tool's output exceeded the capture limit). */
  truncatedAtCapture?: boolean;
  durationMs?: number;
}

interface ResultPayload {
  call_id?: string;
  output_bytes?: number;
  media_type?: string;
  truncated_at_capture?: boolean;
  duration_ms?: number;
}

const rawCache = new Map<string, Promise<RawOutput>>();

/** The `tool.result` event of a call: from the live event, else found in the trace. */
async function findResultEvent(
  sessionId: string,
  callId: string,
  agentId: string | undefined,
): Promise<string | undefined> {
  const known = transcriptOf(sessionId)?.calls[callId]?.eventId;
  if (known !== undefined) return known;
  const params = { session_id: sessionId, kinds: ["tool.result"], limit: 500 };
  const { events } = await call(
    "trace.list",
    agentId === undefined ? params : { ...params, agent_id: agentId },
  );
  for (const ev of events) {
    const cached = transcriptOf(sessionId)?.calls;
    if (cached !== undefined && Object.values(cached).some((c) => c.eventId === ev.id)) continue;
    const got = await call("trace.get", { event_id: ev.id });
    const payload = got.event.payload as ResultPayload;
    if (typeof payload.call_id !== "string") continue;
    const found = payload.call_id;
    update(sessionId, (t) => setCallEvent(t, found, ev.id));
    if (found === callId) return ev.id;
  }
  return undefined;
}

/** The raw output of a tool call from the trace (cached per call). */
export function rawOutput(sessionId: string, callId: string, agentId?: string): Promise<RawOutput> {
  const key = `${sessionId}:${callId}`;
  let p = rawCache.get(key);
  if (p === undefined) {
    p = (async () => {
      const eventId = await findResultEvent(sessionId, callId, agentId);
      if (eventId === undefined) throw new Error("no tool.result event for this call in the trace");
      const got = await call("trace.get", { event_id: eventId, include_blob: true });
      const payload = got.event.payload as ResultPayload;
      const out: RawOutput = { eventId };
      if (got.blob !== undefined) out.text = got.blob;
      if (typeof payload.output_bytes === "number") out.bytes = payload.output_bytes;
      if (typeof payload.media_type === "string") out.mediaType = payload.media_type;
      if (payload.truncated_at_capture === true) out.truncatedAtCapture = true;
      if (typeof payload.duration_ms === "number") out.durationMs = payload.duration_ms;
      return out;
    })();
    rawCache.set(key, p);
    p.catch(() => rawCache.delete(key));
  }
  return p;
}
