// The usage panel's state (task M01-13): the range and chart mode the
// user picked, what `lib/usage.ts` last read, and the request drawer.

import { create } from "zustand";
import type { CallSummary, TokenStats } from "../lib/api";

export type RangePreset = "today" | "7d" | "30d" | "custom";

export interface UsageRange {
  preset: RangePreset;
  /** `YYYY-MM-DD` or empty; used by `custom` only. */
  since: string;
  until: string;
}

export type ChartMode = "cost" | "tokens";

export interface Drawer {
  call: CallSummary;
  /** The pretty request body, once read. */
  body?: string;
  error?: string;
}

export interface UsageState {
  range: UsageRange;
  chart: ChartMode;
  stats?: TokenStats;
  loading: boolean;
  error?: string;
  /** The current page of the calls table. */
  calls: CallSummary[];
  total: number;
  page: number;
  drawer?: Drawer;

  setRange(range: UsageRange): void;
  setChart(chart: ChartMode): void;
  setStats(stats: TokenStats): void;
  setLoading(loading: boolean): void;
  setError(error: string | undefined): void;
  setCalls(calls: CallSummary[], total: number, page: number): void;
  openDrawer(call: CallSummary): void;
  setDrawerBody(body: string | undefined, error: string | undefined): void;
  closeDrawer(): void;
}

export const DEFAULT_RANGE: UsageRange = { preset: "7d", since: "", until: "" };

export const useUsage = create<UsageState>((set) => ({
  range: DEFAULT_RANGE,
  chart: "cost",
  loading: false,
  calls: [],
  total: 0,
  page: 0,

  setRange: (range) => set({ range, page: 0 }),
  setChart: (chart) => set({ chart }),
  setStats: (stats) => set({ stats }),
  setLoading: (loading) => set({ loading }),
  setError: (error) =>
    set((s) => {
      const next = { ...s };
      if (error === undefined) delete next.error;
      else next.error = error;
      return next;
    }, true),
  setCalls: (calls, total, page) => set({ calls, total, page }),
  openDrawer: (call) => set({ drawer: { call } }),
  setDrawerBody: (body, error) =>
    set((s) => {
      if (s.drawer === undefined) return s;
      const drawer: Drawer = { call: s.drawer.call };
      if (body !== undefined) drawer.body = body;
      if (error !== undefined) drawer.error = error;
      return { drawer };
    }),
  closeDrawer: () =>
    set((s) => {
      const next = { ...s };
      delete next.drawer;
      return next;
    }, true),
}));
