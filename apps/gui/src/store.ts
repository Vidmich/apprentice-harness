// App state (Zustand): daemon connection, auth, the model in use, and the
// Playground tabs. Nothing here talks to the daemon — `lib/bootstrap.ts`
// and `lib/playground.ts` do and write the results in.

import { create } from "zustand";
import type { AuthStatusResult } from "./lib/api";
import type { AppInfo, DaemonStatus } from "./lib/rpc";
import { type RunState, idleRun } from "./lib/run";

export interface Tab {
  id: number;
  title: string;
  prompt: string;
  /** Folder for the session; empty = none. */
  workspace: string;
  run: RunState;
}

export interface AppState {
  app: AppInfo | undefined;
  daemon: DaemonStatus;
  /** `undefined` until the first `auth.status` answers. */
  auth: AuthStatusResult | undefined;
  /** `mentor.model` from config, once read. */
  model: string | undefined;
  tabs: Tab[];
  activeTab: number;
  nextTabId: number;

  setApp(app: AppInfo): void;
  setDaemon(status: DaemonStatus): void;
  setAuth(auth: AuthStatusResult | undefined): void;
  setModel(model: string | undefined): void;
  addTab(): number;
  closeTab(id: number): void;
  selectTab(id: number): void;
  updateTab(id: number, patch: Partial<Omit<Tab, "id" | "run">>): void;
  updateRun(id: number, f: (run: RunState) => RunState): void;
}

function newTab(id: number): Tab {
  return { id, title: `Playground ${id}`, prompt: "", workspace: "", run: idleRun() };
}

export const useStore = create<AppState>((set, get) => ({
  app: undefined,
  daemon: { connected: false, spawned: false },
  auth: undefined,
  model: undefined,
  tabs: [newTab(1)],
  activeTab: 1,
  nextTabId: 2,

  setApp: (app) => set({ app }),
  setDaemon: (daemon) => set({ daemon }),
  setAuth: (auth) => set({ auth }),
  setModel: (model) => set({ model }),
  addTab: () => {
    const id = get().nextTabId;
    set((s) => ({ tabs: [...s.tabs, newTab(id)], activeTab: id, nextTabId: id + 1 }));
    return id;
  },
  closeTab: (id) =>
    set((s) => {
      const tabs = s.tabs.filter((t) => t.id !== id);
      if (tabs.length === 0) return s;
      const activeTab =
        s.activeTab === id ? (tabs[tabs.length - 1]?.id ?? s.activeTab) : s.activeTab;
      return { tabs, activeTab };
    }),
  selectTab: (id) => set({ activeTab: id }),
  updateTab: (id, patch) =>
    set((s) => ({ tabs: s.tabs.map((t) => (t.id === id ? { ...t, ...patch } : t)) })),
  updateRun: (id, f) =>
    set((s) => ({ tabs: s.tabs.map((t) => (t.id === id ? { ...t, run: f(t.run) } : t)) })),
}));

/** Whether the mentor provider has a key (undefined while unknown). */
export function authConfigured(auth: AuthStatusResult | undefined): boolean | undefined {
  if (auth === undefined) return undefined;
  return auth.providers.some((p) => p.name === "anthropic" && p.configured);
}
