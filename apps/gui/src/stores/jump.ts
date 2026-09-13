// A pending "show this turn": the usage panel's calls table opens a
// session at the agent of a call (task M01-13). `TranscriptView` scrolls
// to the agent's first item once it is on screen, loading older pages
// on the way, and clears the target.

import { create } from "zustand";

export interface JumpTarget {
  sessionId: string;
  agentId: string;
  /** Older pages loaded looking for the agent; the search gives up at `MAX_PAGES`. */
  pages: number;
}

/** Older pages a jump loads at most before giving up. */
export const MAX_PAGES = 20;

export interface JumpState {
  target?: JumpTarget;
  set(sessionId: string, agentId: string): void;
  /** One more page was requested for the target. */
  paged(): void;
  clear(): void;
}

export const useJump = create<JumpState>((set) => ({
  set: (sessionId, agentId) => set({ target: { sessionId, agentId, pages: 0 } }),
  paged: () =>
    set((s) =>
      s.target === undefined ? s : { target: { ...s.target, pages: s.target.pages + 1 } },
    ),
  clear: () =>
    set((s) => {
      const next = { ...s };
      delete next.target;
      return next;
    }, true),
}));
