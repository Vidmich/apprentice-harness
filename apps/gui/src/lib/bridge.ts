// The backend commands and events as this side sees them: Tauri's
// `invoke` and `listen` in the app, the scripted daemon of `mock.ts`
// when the page runs in a plain browser (`pnpm dev` without `tauri dev`)
// so the chat view can be worked on and eyeballed without the app.

import { invoke as tauriInvoke, isTauri } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";

export type EventHandler<T> = (payload: T) => void;

export interface Backend {
  invoke<T>(command: string, args: Record<string, unknown>): Promise<T>;
  listen<T>(event: string, handler: EventHandler<T>): Promise<() => void>;
}

const tauri: Backend = {
  invoke: (command, args) => tauriInvoke(command, args),
  listen: async (event, handler) => tauriListen<unknown>(event, (e) => handler(e.payload as never)),
};

let backend: Backend | undefined;

/** The backend in use; the mock is loaded on first use outside Tauri. */
export async function getBackend(): Promise<Backend> {
  if (backend !== undefined) return backend;
  if (isTauri()) {
    backend = tauri;
  } else {
    const { mockBackend } = await import("./mock");
    backend = mockBackend();
    console.info("apprentice-harness: not running in Tauri — using the scripted mock daemon");
  }
  return backend;
}

export async function invoke<T>(command: string, args: Record<string, unknown>): Promise<T> {
  return (await getBackend()).invoke<T>(command, args);
}

export async function listen<T>(event: string, handler: EventHandler<T>): Promise<() => void> {
  return (await getBackend()).listen<T>(event, handler);
}

export { isTauri };
