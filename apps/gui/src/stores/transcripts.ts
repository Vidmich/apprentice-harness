// The transcripts in memory, one per session, the five most recently
// shown kept (a session with a run in flight is never evicted: its
// listener writes here). `lib/chat.ts` fills them in; components read.

import { create } from "zustand";
import { type Transcript, emptyTranscript, isBusy } from "../lib/transcript";

export const KEEP = 5;

export interface TranscriptsState {
  transcripts: Record<string, Transcript>;
  /** Least recently shown first. */
  order: string[];

  update(sessionId: string, f: (t: Transcript) => Transcript): void;
  /** Marks the session as shown and evicts idle ones beyond `KEEP`. */
  touch(sessionId: string): void;
  drop(sessionId: string): void;
}

export const useTranscripts = create<TranscriptsState>((set) => ({
  transcripts: {},
  order: [],

  update: (sessionId, f) =>
    set((s) => {
      const current = s.transcripts[sessionId] ?? emptyTranscript(sessionId);
      const next = f(current);
      if (next === current) return s;
      return { transcripts: { ...s.transcripts, [sessionId]: next } };
    }),
  touch: (sessionId) =>
    set((s) => {
      const order = [...s.order.filter((id) => id !== sessionId), sessionId];
      const transcripts = { ...s.transcripts };
      transcripts[sessionId] ??= emptyTranscript(sessionId);
      const keep = new Set(order.slice(-KEEP));
      for (const id of order) {
        const t = transcripts[id];
        if (!keep.has(id) && t !== undefined && !isBusy(t)) delete transcripts[id];
      }
      return { order: order.filter((id) => id in transcripts), transcripts };
    }),
  drop: (sessionId) =>
    set((s) => {
      const transcripts = { ...s.transcripts };
      delete transcripts[sessionId];
      return { transcripts, order: s.order.filter((id) => id !== sessionId) };
    }),
}));

export function transcriptOf(sessionId: string): Transcript | undefined {
  return useTranscripts.getState().transcripts[sessionId];
}
