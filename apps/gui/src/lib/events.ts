// Daemon events, re-emitted by the backend as Tauri events:
// `rpc:event:<channel>` for streaming methods (the channel is a name this
// side picks, so it can listen before the first event exists) and
// `daemon:status` for the connection state.

import { listen } from "@tauri-apps/api/event";
import { type EventNotification, isEventNotification } from "./api";
import type { DaemonStatus } from "./rpc";

export type Unsubscribe = () => void;

export const STATUS_EVENT = "daemon:status";

export function eventName(channel: string): string {
  return `rpc:event:${channel}`;
}

let nextChannel = 1;

/** A fresh channel name for one `stream` call. */
export function newChannel(): string {
  return `${Date.now().toString(36)}-${nextChannel++}`;
}

/** Delivers every event arriving on `channel`, in order, until unsubscribed. */
export async function subscribe(
  channel: string,
  handler: (ev: EventNotification) => void,
): Promise<Unsubscribe> {
  const unlisten = await listen<unknown>(eventName(channel), (e) => {
    if (isEventNotification(e.payload)) handler(e.payload);
  });
  return unlisten;
}

/** Connection-state changes from the backend's connection manager. */
export async function onDaemonStatus(
  handler: (status: DaemonStatus) => void,
): Promise<Unsubscribe> {
  const unlisten = await listen<DaemonStatus>(STATUS_EVENT, (e) => handler(e.payload));
  return unlisten;
}
