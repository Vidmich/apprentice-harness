// The effects behind the sidebar: the workspaces (`workspace.list`,
// `workspace.add`, `workspace.info` for the branch and dirty marker),
// the sessions of the selected one (`session.list`), the search
// (`session.search`), and the lifecycle of a session: open it in a
// chat, rename, archive, delete, export.

import { type Chat, chatOfSession, useStore } from "../store";
import { useSessions } from "../stores/sessions";
import { transcriptOf, useTranscripts } from "../stores/transcripts";
import type { RpcError, SessionExport, SessionSummary, WorkspaceSummary } from "./api";
import { forget, openSession, refreshInfo } from "./chat";
import { RpcFailure, call, describe } from "./rpc";
import { isBusy } from "./transcript";

/** Rows `session.list` is asked for. */
export const LIST_LIMIT = 200;
/** Hits `session.search` is asked for. */
export const SEARCH_LIMIT = 100;
/** The list is re-read this often while the app is shown (runs started elsewhere). */
export const LIST_REFRESH_MS = 30_000;

function toError(e: unknown): RpcError {
  return e instanceof RpcFailure ? e.error : { code: -32603, message: String(e) };
}

// ------------------------------------------------------------- workspaces

/** Re-reads the registered workspaces and the selected one's info. */
export async function refreshWorkspaces(): Promise<void> {
  const store = useStore.getState();
  try {
    const { workspaces } = await call("workspace.list", {});
    store.setWorkspaces(workspaces);
  } catch (e) {
    console.warn("workspace.list failed:", describe(toError(e)));
    return;
  }
  const selected = useStore.getState().selectedWorkspace;
  if (selected !== null) await refreshWorkspaceInfo(selected);
}

/** Reads `workspace.info` (branch, dirty marker, index) of one workspace. */
export async function refreshWorkspaceInfo(id: string): Promise<void> {
  try {
    useStore.getState().setWorkspaceInfo(await call("workspace.info", { id }));
  } catch (e) {
    console.warn("workspace.info failed:", describe(toError(e)));
  }
}

/** Registers a folder (idempotent) and selects it. */
export async function addWorkspace(root: string): Promise<WorkspaceSummary> {
  const added = await call("workspace.add", { root });
  await refreshWorkspaces();
  await selectWorkspace(added.id);
  return added;
}

/** Forgets a workspace (its files and sessions stay). */
export async function removeWorkspace(id: string): Promise<void> {
  await call("workspace.remove", { id });
  await refreshWorkspaces();
  await refreshSessions();
}

/** Selects a workspace (`null` = all) and reloads the list for it. */
export async function selectWorkspace(id: string | null): Promise<void> {
  const store = useStore.getState();
  store.selectWorkspace(id);
  if (id !== null) {
    void refreshWorkspaceInfo(id);
    // The registry orders by use.
    void refreshWorkspaces();
  }
  await refreshSessions();
  const query = useSessions.getState().query;
  if (query.trim() !== "") await searchSessions(query);
}

// --------------------------------------------------------------- the list

let listSeq = 0;

/** Re-reads `session.list` for the selected workspace (or all). */
export async function refreshSessions(): Promise<void> {
  const app = useStore.getState();
  const sessions = useSessions.getState();
  if (!app.daemon.connected) return;
  const seq = ++listSeq;
  sessions.setLoading(true);
  const params: Parameters<typeof call<"session.list">>[1] = {
    include_archived: sessions.showArchived,
    limit: LIST_LIMIT,
  };
  if (app.selectedWorkspace !== null) params.workspace_id = app.selectedWorkspace;
  try {
    const got = await call("session.list", params);
    if (seq !== listSeq) return; // a newer read is on its way
    // `include_archived` also lists deleted rows (traces kept, no
    // conversation); the sidebar has nothing to show for them.
    sessions.setSessions(got.sessions.filter((s) => s.status !== "deleted"));
    sessions.setError(undefined);
  } catch (e) {
    if (seq !== listSeq) return;
    sessions.setError(describe(toError(e)));
  } finally {
    if (seq === listSeq) sessions.setLoading(false);
  }
}

let searchSeq = 0;

/** Runs `session.search` for the box's text (empty clears the hits). */
export async function searchSessions(query: string): Promise<void> {
  const sessions = useSessions.getState();
  const seq = ++searchSeq;
  if (query.trim() === "") {
    sessions.setHits([]);
    sessions.setSearching(false);
    return;
  }
  sessions.setSearching(true);
  try {
    const got = await call("session.search", {
      query,
      include_archived: sessions.showArchived,
      limit: SEARCH_LIMIT,
    });
    if (seq !== searchSeq) return;
    sessions.setHits(got.hits);
  } catch (e) {
    if (seq !== searchSeq) return;
    sessions.setError(describe(toError(e)));
  } finally {
    if (seq === searchSeq) sessions.setSearching(false);
  }
}

export async function setShowArchived(show: boolean): Promise<void> {
  useSessions.getState().setShowArchived(show);
  await refreshSessions();
  const query = useSessions.getState().query;
  if (query.trim() !== "") await searchSessions(query);
}

// -------------------------------------------------------------- lifecycle

/** Shows a session in a chat (opening one), loading it if needed. */
export function showSession(sessionId: string, workspace: string | null | undefined): Chat {
  const store = useStore.getState();
  const before = store.chats.filter((c) => c.sessionId !== undefined).map((c) => c.sessionId!);
  const id = store.openChat(sessionId, workspace ?? "", (sid) => {
    const t = transcriptOf(sid);
    return t !== undefined && isBusy(t);
  });
  // Chats the store closed to make room lose their listeners.
  const after = new Set(useStore.getState().chats.map((c) => c.sessionId));
  for (const sid of before) if (!after.has(sid)) forget(sid);
  useTranscripts.getState().touch(sessionId);
  void openSession(sessionId);
  return useStore.getState().chats.find((c) => c.id === id)!;
}

/** A fresh chat on the selected workspace (`Ctrl+N`). */
export function newSession(): void {
  useStore.getState().addChat();
}

export async function renameSession(sessionId: string, title: string): Promise<void> {
  const trimmed = title.trim();
  if (trimmed === "") return;
  await call("session.rename", { id: sessionId, title: trimmed });
  useSessions.getState().patch(sessionId, { title: trimmed });
  if (transcriptOf(sessionId) !== undefined) void refreshInfo(sessionId);
  void refreshSessions();
}

export async function archiveSession(sessionId: string, archived: boolean): Promise<void> {
  await call("session.archive", { id: sessionId, archived });
  const chat = chatOfSession(sessionId);
  if (chat !== undefined && archived) {
    forget(sessionId);
    useStore.getState().closeChat(chat.id);
  }
  await refreshSessions();
}

export async function deleteSession(sessionId: string, purgeTraces = false): Promise<void> {
  await call("session.delete", { id: sessionId, purge_traces: purgeTraces });
  const chat = chatOfSession(sessionId);
  if (chat !== undefined) {
    forget(sessionId);
    useStore.getState().closeChat(chat.id);
  }
  await refreshSessions();
}

/** The session as one JSON document. */
export async function exportSession(sessionId: string): Promise<SessionExport> {
  return call("session.export", { id: sessionId });
}

/** The row's title, the CLI way: the title, else the id. */
export function titleOf(row: Pick<SessionSummary, "id" | "title">): string {
  return row.title ?? row.id;
}
