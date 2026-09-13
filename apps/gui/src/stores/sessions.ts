// The sessions list of the sidebar and its search, as `lib/sessions.ts`
// last read them.

import { create } from "zustand";
import type { SessionSearchHit, SessionSummary } from "../lib/api";

export interface SessionsState {
  /** `session.list` for the selected workspace (or all), newest activity first. */
  sessions: SessionSummary[];
  loading: boolean;
  error?: string;
  /** The search box; empty shows the list. */
  query: string;
  hits: SessionSearchHit[];
  searching: boolean;
  showArchived: boolean;

  setSessions(sessions: SessionSummary[]): void;
  setLoading(loading: boolean): void;
  setError(error: string | undefined): void;
  setQuery(query: string): void;
  setHits(hits: SessionSearchHit[]): void;
  setSearching(searching: boolean): void;
  setShowArchived(show: boolean): void;
  /** Patches one row in place (a rename, a status) until the next list. */
  patch(sessionId: string, patch: Partial<SessionSummary>): void;
}

export const useSessions = create<SessionsState>((set) => ({
  sessions: [],
  loading: false,
  query: "",
  hits: [],
  searching: false,
  showArchived: false,

  setSessions: (sessions) => set({ sessions }),
  setLoading: (loading) => set({ loading }),
  setError: (error) =>
    set((s) => {
      const next = { ...s };
      if (error === undefined) delete next.error;
      else next.error = error;
      return next;
    }, true),
  setQuery: (query) => set({ query }),
  setHits: (hits) => set({ hits }),
  setSearching: (searching) => set({ searching }),
  setShowArchived: (showArchived) => set({ showArchived }),
  patch: (sessionId, patch) =>
    set((s) => ({
      sessions: s.sessions.map((row) => (row.id === sessionId ? { ...row, ...patch } : row)),
    })),
}));

export function sessionRow(sessionId: string): SessionSummary | undefined {
  return useSessions.getState().sessions.find((s) => s.id === sessionId);
}
