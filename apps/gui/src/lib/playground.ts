// Runs a Playground tab's prompt: fresh session → `agent.run` → events
// folded into the tab's `RunState`. Each tab has its own subscription, so
// concurrent runs never share a listener.

import { useStore } from "../store";
import { type Unsubscribe, newChannel, subscribe } from "./events";
import { RpcFailure, call, stream } from "./rpc";
import { applyEvent, attachRun, cancellingRun, failRun, startingRun } from "./run";

const listeners = new Map<number, Unsubscribe>();

function stopListening(tabId: number): void {
  listeners.get(tabId)?.();
  listeners.delete(tabId);
}

/** Starts the tab's prompt. Resolves when the run has been accepted. */
export async function startRun(tabId: number): Promise<void> {
  const store = useStore.getState();
  const tab = store.tabs.find((t) => t.id === tabId);
  if (tab === undefined || tab.run.phase === "running" || tab.run.phase === "cancelling") return;
  const prompt = tab.prompt.trim();
  if (prompt === "") return;
  stopListening(tabId);

  let sessionId: string;
  try {
    const params = tab.workspace.trim() === "" ? {} : { workspace: tab.workspace.trim() };
    sessionId = (await call("session.create", { ...params, title: firstLine(prompt) })).session_id;
  } catch (e) {
    const error = e instanceof RpcFailure ? e.error : { code: -32603, message: String(e) };
    store.updateRun(tabId, () => failRun(startingRun("", Date.now()), error, Date.now()));
    return;
  }
  store.updateRun(tabId, () => startingRun(sessionId, Date.now()));

  // Listen first, then call: nothing can slip past between the two.
  const channel = newChannel();
  const stop = await subscribe(channel, (ev) => {
    useStore.getState().updateRun(tabId, (run) => applyEvent(run, ev, Date.now()));
    if (ev.event.type === "agent.finished") stopListening(tabId);
  });
  listeners.set(tabId, stop);
  try {
    const started = await stream("agent.run", { session_id: sessionId, prompt }, channel);
    store.updateRun(tabId, (run) => attachRun(run, started.result.agent_id, started.subscription));
  } catch (e) {
    stopListening(tabId);
    const error = e instanceof RpcFailure ? e.error : { code: -32603, message: String(e) };
    store.updateRun(tabId, (run) => failRun(run, error, Date.now()));
  }
}

/** Asks the daemon to cancel; the run finishes with `cancelled` via events. */
export async function cancelRun(tabId: number): Promise<void> {
  const store = useStore.getState();
  const tab = store.tabs.find((t) => t.id === tabId);
  const agentId = tab?.run.agentId;
  if (tab === undefined || agentId === undefined || tab.run.phase !== "running") return;
  store.updateRun(tabId, cancellingRun);
  try {
    await call("agent.cancel", { agent_id: agentId });
  } catch (e) {
    const error = e instanceof RpcFailure ? e.error : { code: -32603, message: String(e) };
    store.updateRun(tabId, (run) => failRun(run, error, Date.now()));
    stopListening(tabId);
  }
}

/** Drops the tab's listener (on close). A running agent keeps running. */
export function forgetRun(tabId: number): void {
  stopListening(tabId);
}

function firstLine(prompt: string): string {
  const line = prompt.split("\n", 1)[0] ?? prompt;
  return line.length > 60 ? `${line.slice(0, 57)}...` : line;
}
