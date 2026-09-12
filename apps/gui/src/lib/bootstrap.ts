// Wires the backend to the store: app info once, the daemon connection
// state as it changes, and auth/model whenever the daemon (re)connects.

import { useStore } from "../store";
import { onDaemonStatus } from "./events";
import { RpcFailure, appInfo, call, daemonStatus } from "./rpc";

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
  return stop;
}
