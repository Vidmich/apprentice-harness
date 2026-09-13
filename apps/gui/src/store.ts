// App state (Zustand): daemon connection, auth, the model in use, the
// workspaces and which is selected, the chats (one per session) and
// the composer settings. Nothing here talks to the daemon —
// `lib/bootstrap.ts`, `lib/chat.ts` and `lib/sessions.ts` do and write
// the results in. The chats, the selected workspace and the settings
// survive a reload (localStorage) so the app comes back where it was.

import { create } from "zustand";
import type {
  AuthStatusResult,
  ThinkingDisplay,
  WorkspaceInfoResult,
  WorkspaceSummary,
} from "./lib/api";
import type { AppInfo, DaemonStatus } from "./lib/rpc";

/** What sends the composer: `Enter` (Shift+Enter breaks the line) or `Ctrl+Enter`. */
export type SendKey = "enter" | "ctrl_enter";

/** What the main pane shows. */
export type View = "chat" | "settings";

export interface Chat {
  id: number;
  /** Set once the first prompt created the session (or the chat opened one). */
  sessionId?: string;
  /** Folder for the session; empty = none chosen yet. */
  workspace: string;
  /** Why the last send never reached a session (`session.create` failed). */
  error?: string;
}

/** `error: undefined` clears the chat's error. */
export interface ChatPatch {
  sessionId?: string;
  workspace?: string;
  error?: string | undefined;
}

/** Chats kept open beyond the active one (idle ones past this are closed). */
export const MAX_CHATS = 8;

interface Persisted {
  chats: Chat[];
  activeChat: number;
  nextChatId: number;
  sendKey: SendKey;
  /** Workspace id the sidebar shows; `null` = all workspaces. */
  selectedWorkspace: string | null;
}

export interface AppState extends Persisted {
  app: AppInfo | undefined;
  daemon: DaemonStatus;
  /** `undefined` until the first `auth.status` answers. */
  auth: AuthStatusResult | undefined;
  /** `mentor.model` from config, once read. */
  model: string | undefined;
  /** `mentor.thinking_display` from config; thinking is shown until known otherwise. */
  thinkingDisplay: ThinkingDisplay;
  view: View;
  /** `workspace.list`, most recently used first. */
  workspaces: WorkspaceSummary[];
  /** `workspace.info` by id, for the ones looked at. */
  workspaceInfo: Record<string, WorkspaceInfoResult>;

  setApp(app: AppInfo): void;
  setDaemon(status: DaemonStatus): void;
  setAuth(auth: AuthStatusResult | undefined): void;
  setModel(model: string | undefined): void;
  setThinkingDisplay(display: ThinkingDisplay): void;
  setView(view: View): void;
  setWorkspaces(workspaces: WorkspaceSummary[]): void;
  setWorkspaceInfo(info: WorkspaceInfoResult): void;
  selectWorkspace(id: string | null): void;
  /** A fresh chat on `workspace` (the selected one's root by default). */
  addChat(workspace?: string): number;
  /**
   * Shows the chat of a session, opening one when none is. Idle chats
   * beyond `MAX_CHATS` go; `busy` says which must stay.
   */
  openChat(sessionId: string, workspace: string, busy: (sessionId: string) => boolean): number;
  closeChat(id: number): void;
  selectChat(id: number): void;
  updateChat(id: number, patch: ChatPatch): void;
  setSendKey(key: SendKey): void;
}

const STORAGE_KEY = "harness.chats";

function newChat(id: number, workspace = ""): Chat {
  return { id, workspace };
}

function defaults(): Persisted {
  return {
    chats: [newChat(1)],
    activeChat: 1,
    nextChatId: 2,
    sendKey: "enter",
    selectedWorkspace: null,
  };
}

/** What the last session left in localStorage, or the defaults. */
function restore(): Persisted {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === null) return defaults();
    const v = JSON.parse(raw) as Partial<Persisted>;
    const chats = Array.isArray(v.chats)
      ? v.chats
          .filter((c): c is Chat => typeof c === "object" && c !== null && typeof c.id === "number")
          .map((c) => {
            const chat: Chat = {
              id: c.id,
              workspace: typeof c.workspace === "string" ? c.workspace : "",
            };
            if (typeof c.sessionId === "string") chat.sessionId = c.sessionId;
            return chat;
          })
      : [];
    if (chats.length === 0) return defaults();
    const activeChat = chats.some((c) => c.id === v.activeChat)
      ? (v.activeChat as number)
      : chats[0]!.id;
    const nextChatId = Math.max(...chats.map((c) => c.id)) + 1;
    return {
      chats,
      activeChat,
      nextChatId,
      sendKey: v.sendKey === "ctrl_enter" ? "ctrl_enter" : "enter",
      selectedWorkspace: typeof v.selectedWorkspace === "string" ? v.selectedWorkspace : null,
    };
  } catch {
    return defaults();
  }
}

function persist(s: Persisted): void {
  try {
    const chats = s.chats.map(({ id, sessionId, workspace }) =>
      sessionId === undefined ? { id, workspace } : { id, sessionId, workspace },
    );
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        chats,
        activeChat: s.activeChat,
        nextChatId: s.nextChatId,
        sendKey: s.sendKey,
        selectedWorkspace: s.selectedWorkspace,
      }),
    );
  } catch {
    // A full or disabled storage only costs the reload experience.
  }
}

export const useStore = create<AppState>((set, get) => ({
  ...restore(),
  app: undefined,
  daemon: { connected: false, spawned: false },
  auth: undefined,
  model: undefined,
  thinkingDisplay: "summarized",
  view: "chat",
  workspaces: [],
  workspaceInfo: {},

  setApp: (app) => set({ app }),
  setDaemon: (daemon) => set({ daemon }),
  setAuth: (auth) => set({ auth }),
  setModel: (model) => set({ model }),
  setThinkingDisplay: (thinkingDisplay) => set({ thinkingDisplay }),
  setView: (view) => set({ view }),
  setWorkspaces: (workspaces) =>
    set((s) => ({
      workspaces,
      // A forgotten workspace cannot stay selected.
      selectedWorkspace:
        s.selectedWorkspace !== null && !workspaces.some((w) => w.id === s.selectedWorkspace)
          ? null
          : s.selectedWorkspace,
    })),
  setWorkspaceInfo: (info) =>
    set((s) => ({ workspaceInfo: { ...s.workspaceInfo, [info.id]: info } })),
  selectWorkspace: (selectedWorkspace) => set({ selectedWorkspace }),
  addChat: (workspace) => {
    const s = get();
    const root = workspace ?? s.workspaces.find((w) => w.id === s.selectedWorkspace)?.root ?? "";
    // One empty draft per workspace is enough.
    const draft = s.chats.find((c) => c.sessionId === undefined && c.workspace === root);
    if (draft !== undefined) {
      set({ activeChat: draft.id, view: "chat" });
      return draft.id;
    }
    const id = s.nextChatId;
    set({
      chats: [...s.chats, newChat(id, root)],
      activeChat: id,
      nextChatId: id + 1,
      view: "chat",
    });
    return id;
  },
  openChat: (sessionId, workspace, busy) => {
    const s = get();
    const existing = s.chats.find((c) => c.sessionId === sessionId);
    if (existing !== undefined) {
      set({ activeChat: existing.id, view: "chat" });
      return existing.id;
    }
    const id = s.nextChatId;
    const chat: Chat = { id, sessionId, workspace };
    // Oldest idle chats go first; drafts with text typed are the
    // composer's business, so only session chats are closed.
    const keep: Chat[] = [];
    let extra = s.chats.filter((c) => c.sessionId !== undefined).length + 1 - MAX_CHATS;
    for (const c of s.chats) {
      if (extra > 0 && c.sessionId !== undefined && !busy(c.sessionId)) {
        extra -= 1;
        continue;
      }
      keep.push(c);
    }
    set({ chats: [...keep, chat], activeChat: id, nextChatId: id + 1, view: "chat" });
    return id;
  },
  closeChat: (id) =>
    set((s) => {
      const chats = s.chats.filter((c) => c.id !== id);
      if (chats.length === 0) {
        const fresh = newChat(s.nextChatId);
        return { chats: [fresh], activeChat: fresh.id, nextChatId: fresh.id + 1 };
      }
      const activeChat =
        s.activeChat === id ? (chats[chats.length - 1]?.id ?? s.activeChat) : s.activeChat;
      return { chats, activeChat };
    }),
  selectChat: (id) => set({ activeChat: id, view: "chat" }),
  updateChat: (id, patch) =>
    set((s) => ({
      chats: s.chats.map((c) => {
        if (c.id !== id) return c;
        const next: Chat = { ...c };
        if (patch.sessionId !== undefined) next.sessionId = patch.sessionId;
        if (patch.workspace !== undefined) next.workspace = patch.workspace;
        if ("error" in patch) {
          if (patch.error === undefined) delete next.error;
          else next.error = patch.error;
        }
        return next;
      }),
    })),
  setSendKey: (sendKey) => set({ sendKey }),
}));

useStore.subscribe((s, prev) => {
  if (
    s.chats !== prev.chats ||
    s.activeChat !== prev.activeChat ||
    s.nextChatId !== prev.nextChatId ||
    s.sendKey !== prev.sendKey ||
    s.selectedWorkspace !== prev.selectedWorkspace
  ) {
    persist(s);
  }
});

export function chatOf(id: number): Chat | undefined {
  return useStore.getState().chats.find((c) => c.id === id);
}

/** The chat showing `sessionId`, if one is open. */
export function chatOfSession(sessionId: string): Chat | undefined {
  return useStore.getState().chats.find((c) => c.sessionId === sessionId);
}

/** The selected workspace's row, if one is selected. */
export function selectedWorkspace(s: AppState): WorkspaceSummary | undefined {
  return s.selectedWorkspace === null
    ? undefined
    : s.workspaces.find((w) => w.id === s.selectedWorkspace);
}

/** Whether the mentor provider has a key (undefined while unknown). */
export function authConfigured(auth: AuthStatusResult | undefined): boolean | undefined {
  if (auth === undefined) return undefined;
  return auth.providers.some((p) => p.name === "anthropic" && p.configured);
}
