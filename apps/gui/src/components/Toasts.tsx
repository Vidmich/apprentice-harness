import { useMemo } from "react";
import { showSession, titleOf } from "../lib/sessions";
import { useStore } from "../store";
import { usePermissions } from "../stores/permissions";
import { useSessions } from "../stores/sessions";
import { useTranscripts } from "../stores/transcripts";

/**
 * Non-modal notices for permission requests of sessions not in front:
 * one per session, with the count and a button that brings the session
 * up (the dialog then opens there).
 */
export default function Toasts({ activeSession }: { activeSession: string | undefined }) {
  const state = usePermissions((s) => s.state);
  const sessions = useSessions((s) => s.sessions);
  const chats = useStore((s) => s.chats);
  const transcripts = useTranscripts((s) => s.transcripts);

  const notices = useMemo(() => {
    const bySession = new Map<string, { count: number; tool: string; description: string }>();
    for (const id of state.order) {
      const p = state.pending[id];
      if (p === undefined || p.sessionId === activeSession) continue;
      const cur = bySession.get(p.sessionId);
      if (cur === undefined) {
        bySession.set(p.sessionId, { count: 1, tool: p.tool, description: p.description });
      } else cur.count += 1;
    }
    return [...bySession.entries()];
  }, [state, activeSession]);

  if (notices.length === 0) return null;
  return (
    <div
      className="pointer-events-none absolute right-4 bottom-4 z-20 flex flex-col gap-2"
      role="status"
    >
      {notices.map(([sessionId, n]) => {
        const row = sessions.find((s) => s.id === sessionId);
        const workspace = row?.workspace ?? chats.find((c) => c.sessionId === sessionId)?.workspace;
        const title =
          row !== undefined ? titleOf(row) : (transcripts[sessionId]?.info?.title ?? sessionId);
        return (
          <div
            key={sessionId}
            className="pointer-events-auto flex max-w-sm items-center gap-3 rounded border border-warn bg-panel px-3 py-2 text-sm shadow-lg"
          >
            <div className="min-w-0">
              <div className="truncate font-medium">{title}</div>
              <div className="truncate text-xs text-muted">
                asks for <span className="font-mono">{n.tool}</span>
                {n.count > 1 ? ` (+${n.count - 1} more)` : ""}
                {n.description !== "" ? ` — ${n.description}` : ""}
              </div>
            </div>
            <button
              type="button"
              className="btn-primary shrink-0 py-0 text-xs"
              onClick={() => showSession(sessionId, workspace)}
            >
              Show
            </button>
          </div>
        );
      })}
    </div>
  );
}
