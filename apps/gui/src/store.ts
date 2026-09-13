// App state (Zustand): daemon connection, auth, the model in use, the
// chats (one per session) and the composer settings. Nothing here talks
// to the daemon — `lib/bootstrap.ts` and `lib/chat.ts` do and write the
// results in. The chats and settings survive a reload (localStorage) so
// a running session is found again.

import { create } from "zustand";
import type { AuthStatusResult, ThinkingDisplay } from "./lib/api";
import type { AppInfo, DaemonStatus } from "./lib/rpc";

/** What sends the composer: `Enter` (Shift+Enter breaks the line) or `Ctrl+Enter`. */
export type SendKey = "enter" | "ctrl_enter";

export interface Chat {
  id: number;
  /** Set once the first prompt created the session. */
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

interface Persisted {
  chats: Chat[];
  activeChat: number;
  nextChatId: number;
  sendKey: SendKey;
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

  setApp(app: AppInfo): void;
  setDaemon(status: DaemonStatus): void;
  setAuth(auth: AuthStatusResult | undefined): void;
  setModel(model: string | undefined): void;
  setThinkingDisplay(display: ThinkingDisplay): void;
  addChat(): number;
  closeChat(id: number): void;
  selectChat(id: number): void;
  updateChat(id: number, patch: ChatPatch): void;
  setSendKey(key: SendKey): void;
}

const STORAGE_KEY = "harness.chats";

function newChat(id: number): Chat {
  return { id, workspace: "" };
}

function defaults(): Persisted {
  return { chats: [newChat(1)], activeChat: 1, nextChatId: 2, sendKey: "enter" };
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

  setApp: (app) => set({ app }),
  setDaemon: (daemon) => set({ daemon }),
  setAuth: (auth) => set({ auth }),
  setModel: (model) => set({ model }),
  setThinkingDisplay: (thinkingDisplay) => set({ thinkingDisplay }),
  addChat: () => {
    const id = get().nextChatId;
    set((s) => ({ chats: [...s.chats, newChat(id)], activeChat: id, nextChatId: id + 1 }));
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
  selectChat: (id) => set({ activeChat: id }),
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
    s.sendKey !== prev.sendKey
  ) {
    persist(s);
  }
});

export function chatOf(id: number): Chat | undefined {
  return useStore.getState().chats.find((c) => c.id === id);
}

/** Whether the mentor provider has a key (undefined while unknown). */
export function authConfigured(auth: AuthStatusResult | undefined): boolean | undefined {
  if (auth === undefined) return undefined;
  return auth.providers.some((p) => p.name === "anthropic" && p.configured);
}
