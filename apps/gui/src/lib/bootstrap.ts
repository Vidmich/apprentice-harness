// Wires the backend to the store: app info once, the daemon connection
// state as it changes, and auth/model/workspaces/sessions whenever the
// daemon (re)connects (the list again every `LIST_REFRESH_MS` while the
// window is shown, for runs started elsewhere).

import { useStore } from "../store";
import { onDaemonStatus } from "./events";
import { RpcFailure, appInfo, call, daemonStatus } from "./rpc";
import { LIST_REFRESH_MS, refreshSessions, refreshWorkspaces } from "./sessions";

/** Re-reads what the status bar and Setup screen show. */
export async function refreshDaemonFacts(): Promise<void> {
  const store = useStore.getState();
  try {
    store.setAuth(await call("auth.status", {}));
  } catch (e) {
    console.warn("auth.status failed:", e instanceof RpcFailure ? e.message : e);
    store.setAuth(undefined);
  }
  try {
    const got = await call("config.get", { key: "mentor.model" });
    store.setModel(typeof got.value === "string" ? got.value : undefined);
  } catch (e) {
    console.warn("config.get failed:", e instanceof RpcFailure ? e.message : e);
    store.setModel(undefined);
  }
  try {
    const got = await call("config.get", { key: "mentor.thinking_display" });
    store.setThinkingDisplay(got.value === "omitted" ? "omitted" : "summarized");
  } catch (e) {
    console.warn("config.get failed:", e instanceof RpcFailure ? e.message : e);
  }
  await refreshWorkspaces();
  await refreshSessions();
}

/** Called once from `main.tsx`. Returns a function that stops listening. */
export async function bootstrap(): Promise<() => void> {
  const store = useStore.getState();
  const stop = await onDaemonStatus((status) => {
    const was = useStore.getState().daemon.connected;
    store.setDaemon(status);
    if (status.connected && !was) void refreshDaemonFacts();
    if (!status.connected) store.setAuth(undefined);
  });
  try {
    store.setApp(await appInfo());
  } catch (e) {
    console.warn("app_info failed:", e);
  }
  // The manager may have connected before we started listening.
  try {
    const status = await daemonStatus();
    store.setDaemon(status);
    if (status.connected) await refreshDaemonFacts();
  } catch (e) {
    console.warn("daemon_status failed:", e);
  }
  const timer = setInterval(() => {
    if (document.visibilityState === "visible") void refreshSessions();
  }, LIST_REFRESH_MS);
  return () => {
    clearInterval(timer);
    stop();
  };
}
