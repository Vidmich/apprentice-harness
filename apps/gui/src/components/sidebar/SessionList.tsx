import { useEffect, useMemo, useState } from "react";
import type { SessionSearchHit, SessionSummary } from "../../lib/api";
import { type DayGroup, ago, dayGroup } from "../../lib/format";
import { countBySession } from "../../lib/permissions";
import { searchSessions, setShowArchived, showSession } from "../../lib/sessions";
import { isBusy } from "../../lib/transcript";
import { selectedWorkspace, useStore } from "../../store";
import { usePermissions } from "../../stores/permissions";
import { useSessions } from "../../stores/sessions";
import { useTranscripts } from "../../stores/transcripts";
import SessionRow from "./SessionRow";

const GROUPS: DayGroup[] = ["Today", "Yesterday", "Earlier"];
const SEARCH_DEBOUNCE_MS = 250;

/**
 * The sessions of the selected workspace grouped by day, with the
 * drafts (chats without a session yet) on top, or the search hits when
 * the box has text.
 */
export default function SessionList() {
  const sessions = useSessions((s) => s.sessions);
  const loading = useSessions((s) => s.loading);
  const error = useSessions((s) => s.error);
  const query = useSessions((s) => s.query);
  const setQuery = useSessions((s) => s.setQuery);
  const hits = useSessions((s) => s.hits);
  const searching = useSessions((s) => s.searching);
  const showArchived = useSessions((s) => s.showArchived);
  const chats = useStore((s) => s.chats);
  const activeChat = useStore((s) => s.activeChat);
  const view = useStore((s) => s.view);
  const selectChat = useStore((s) => s.selectChat);
  const selected = useStore(selectedWorkspace);
  const transcripts = useTranscripts((s) => s.transcripts);
  const permissions = usePermissions((s) => s.state);
  const [rowError, setRowError] = useState<string>();

  // The search runs a moment after the last keystroke.
  useEffect(() => {
    const timer = setTimeout(() => void searchSessions(query), SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [query]);

  const asking = useMemo(() => countBySession(permissions), [permissions]);
  const activeSession =
    view === "chat" ? chats.find((c) => c.id === activeChat)?.sessionId : undefined;
  const drafts = chats.filter((c) => c.sessionId === undefined);

  const stateOf = (row: SessionSummary) => {
    const t = transcripts[row.id];
    return {
      active: row.id === activeSession,
      running: row.running_agent !== undefined || (t !== undefined && isBusy(t)),
      asking: asking[row.id] ?? 0,
    };
  };

  const grouped = useMemo(() => {
    const now = new Date();
    const out: Record<DayGroup, SessionSummary[]> = { Today: [], Yesterday: [], Earlier: [] };
    for (const row of sessions) out[dayGroup(row.last_activity, now)].push(row);
    return out;
  }, [sessions]);

  const searchMode = query.trim() !== "";
  const visibleHits = useMemo(
    () =>
      selected === undefined
        ? hits
        : hits.filter((h) => h.workspace === undefined || h.workspace === selected.root),
    [hits, selected],
  );

  return (
    <section className="flex min-h-0 grow flex-col gap-2">
      <div className="flex items-center gap-1">
        <input
          className="input min-w-0 grow"
          type="search"
          placeholder="Search sessions"
          aria-label="Search sessions"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.stopPropagation();
              setQuery("");
            }
          }}
        />
      </div>
      <label className="flex items-center gap-1 px-1 text-[11px] text-muted">
        <input
          type="checkbox"
          checked={showArchived}
          onChange={(e) => void setShowArchived(e.target.checked)}
        />
        show archived
      </label>
      {(error ?? rowError) !== undefined && (
        <p className="px-1 text-xs text-bad" role="alert">
          {error ?? rowError}
        </p>
      )}
      <div className="min-h-0 grow overflow-y-auto">
        {searchMode ? (
          <SearchResults
            hits={visibleHits}
            searching={searching}
            activeSession={activeSession}
            onOpen={(h) => showSession(h.session_id, h.workspace)}
          />
        ) : (
          <>
            {drafts.length > 0 && (
              <ul className="mb-2 flex flex-col gap-0.5">
                {drafts.map((c) => (
                  <li
                    key={c.id}
                    className={`cursor-pointer truncate rounded px-2 py-1 text-sm italic ${
                      view === "chat" && c.id === activeChat ? "bg-bg" : "hover:bg-bg/50"
                    }`}
                    onClick={() => selectChat(c.id)}
                    aria-current={view === "chat" && c.id === activeChat ? "page" : undefined}
                    title={c.workspace === "" ? "no workspace chosen" : c.workspace}
                  >
                    New session
                  </li>
                ))}
              </ul>
            )}
            {GROUPS.map((g) =>
              grouped[g].length === 0 ? null : (
                <div key={g} className="mb-2">
                  <h3 className="px-2 py-1 text-[10px] tracking-wider text-muted uppercase">{g}</h3>
                  <ul className="flex flex-col gap-0.5">
                    {grouped[g].map((row) => (
                      <SessionRow
                        key={row.id}
                        row={row}
                        state={stateOf(row)}
                        onOpen={() => showSession(row.id, row.workspace)}
                        onError={setRowError}
                      />
                    ))}
                  </ul>
                </div>
              ),
            )}
            {sessions.length === 0 && !loading && (
              <p className="px-2 py-4 text-center text-xs text-muted">
                {selected === undefined
                  ? "No sessions yet."
                  : `No sessions on ${selected.name} yet.`}
              </p>
            )}
          </>
        )}
      </div>
    </section>
  );
}

function SearchResults({
  hits,
  searching,
  activeSession,
  onOpen,
}: {
  hits: SessionSearchHit[];
  searching: boolean;
  activeSession: string | undefined;
  onOpen: (hit: SessionSearchHit) => void;
}) {
  if (hits.length === 0) {
    return (
      <p className="px-2 py-4 text-center text-xs text-muted">
        {searching ? "searching…" : "No matching messages."}
      </p>
    );
  }
  return (
    <ul className="flex flex-col gap-0.5" aria-label="Search results">
      {hits.map((h) => (
        <li
          key={`${h.session_id}:${h.seq}`}
          className={`cursor-pointer rounded px-2 py-1 text-sm ${
            h.session_id === activeSession ? "bg-bg" : "hover:bg-bg/50"
          }`}
          onClick={() => onOpen(h)}
          title={`${h.workspace ?? ""}\n${h.session_id} #${h.seq}`}
        >
          <div className="flex items-center gap-2">
            <span className="grow truncate">{h.title ?? h.session_id}</span>
            <span className="shrink-0 text-[10px] text-muted">{ago(h.created_at)}</span>
          </div>
          <Snippet text={h.snippet} role={h.role} />
        </li>
      ))}
    </ul>
  );
}

/** The daemon marks matches with `[` `]`; they become `<mark>`. */
function Snippet({ text, role }: { text: string; role: string }) {
  const parts = text.split(/(\[[^\]]*\])/);
  return (
    <p className="truncate text-[11px] text-muted">
      <span className="mr-1 text-[10px] uppercase">{role}</span>
      {parts.map((p, i) =>
        p.startsWith("[") && p.endsWith("]") ? (
          <mark key={i} className="rounded bg-warn/30 px-0.5 text-fg">
            {p.slice(1, -1)}
          </mark>
        ) : (
          <span key={i}>{p}</span>
        ),
      )}
    </p>
  );
}
