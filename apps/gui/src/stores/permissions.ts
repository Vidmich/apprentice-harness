// The permission requests waiting for an answer, every session's. The
// event loop of `lib/chat.ts` folds them in; the dialog and the sidebar
// read.

import { create } from "zustand";
import type { Event } from "../lib/api";
import {
  EMPTY_PERMISSIONS,
  type PermissionState,
  applyPermissionEvent,
  expired,
  withoutRequest,
} from "../lib/permissions";

export interface PermissionsStore {
  state: PermissionState;
  apply(sessionId: string, event: Event, now: number): void;
  remove(requestId: string): void;
  /** Drops what the daemon has timed out by `now`. */
  expire(now: number): void;
}

export const usePermissions = create<PermissionsStore>((set) => ({
  state: EMPTY_PERMISSIONS,
  apply: (sessionId, event, now) =>
    set((s) => {
      const next = applyPermissionEvent(s.state, sessionId, event, now);
      return next === s.state ? s : { state: next };
    }),
  remove: (requestId) =>
    set((s) => {
      const next = withoutRequest(s.state, requestId);
      return next === s.state ? s : { state: next };
    }),
  expire: (now) =>
    set((s) => {
      const next = expired(s.state, now);
      return next === s.state ? s : { state: next };
    }),
}));
