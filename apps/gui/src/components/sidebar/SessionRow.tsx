import { type KeyboardEvent, type MouseEvent, useEffect, useRef, useState } from "react";
import type { SessionSummary } from "../../lib/api";
import { ago, usd } from "../../lib/format";
import { RpcFailure } from "../../lib/rpc";
import {
  archiveSession,
  deleteSession,
  exportSession,
  renameSession,
  titleOf,
} from "../../lib/sessions";
import { saveTextFile } from "../../lib/ui";

export interface RowState {
  active: boolean;
  /** A run is in flight (here or in another client). */
  running: boolean;
  /** Permission requests waiting on this session. */
  asking: number;
}

/** One session of the list: title, time, status dot, cost, and a menu. */
export default function SessionRow({
  row,
  state,
  onOpen,
  onError,
}: {
  row: SessionSummary;
  state: RowState;
  onOpen: () => void;
  onError: (message: string) => void;
}) {
  const [menu, setMenu] = useState<{ x: number; y: number }>();
  const [renaming, setRenaming] = useState(false);
  const title = titleOf(row);
  const archived = row.status === "archived";

  const openMenu = (e: MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setMenu({ x: e.clientX, y: e.clientY });
  };

  const run = (f: () => Promise<unknown>) => {
    setMenu(undefined);
    f().catch((e: unknown) => onError(e instanceof RpcFailure ? e.message : String(e)));
  };

  const doExport = () =>
    run(async () => {
      const doc = await exportSession(row.id);
      await saveTextFile(`${row.id}.json`, JSON.stringify(doc, null, 2));
    });

  let dot: React.ReactNode = null;
  if (state.running) {
    dot = (
      <span className="h-2 w-2 shrink-0 animate-pulse rounded-full bg-accent" title="running" />
    );
  } else if (row.last_agent_status === "error") {
    dot = <span className="h-2 w-2 shrink-0 rounded-full bg-bad" title="the last run failed" />;
  } else if (row.last_agent_status === "cancelled") {
    dot = (
      <span className="h-2 w-2 shrink-0 rounded-full bg-muted" title="the last run was cancelled" />
    );
  }

  return (
    <li
      className={`group flex cursor-pointer items-center gap-1.5 rounded px-2 py-1 text-sm ${
        state.active ? "bg-bg" : "hover:bg-bg/50"
      } ${archived ? "opacity-70" : ""}`}
      onClick={onOpen}
      onContextMenu={openMenu}
      aria-current={state.active ? "page" : undefined}
      title={`${title}\n${row.workspace ?? "(no workspace)"}\n${row.id}`}
    >
      {dot}
      {renaming ? (
        <RenameInput
          initial={title}
          onDone={(next) => {
            setRenaming(false);
            if (next !== undefined && next !== title) run(() => renameSession(row.id, next));
          }}
        />
      ) : (
        <span className="grow truncate">{title}</span>
      )}
      {state.asking > 0 && (
        <span
          className="shrink-0 rounded bg-warn px-1 text-[10px] font-semibold text-white"
          title="waiting for a permission answer"
        >
          {state.asking}
        </span>
      )}
      {row.cost_usd !== undefined && row.cost_usd > 0 && (
        <span className="shrink-0 font-mono text-[10px] text-muted">{usd(row.cost_usd)}</span>
      )}
      <span className="shrink-0 text-[10px] text-muted">{ago(row.last_activity)}</span>
      <button
        type="button"
        className="shrink-0 text-xs text-muted opacity-0 group-hover:opacity-100 hover:text-fg focus:opacity-100"
        aria-label={`Actions for ${title}`}
        onClick={openMenu}
      >
        …
      </button>
      {menu !== undefined && (
        <Menu
          at={menu}
          onClose={() => setMenu(undefined)}
          items={[
            { label: "Rename", onSelect: () => (setMenu(undefined), setRenaming(true)) },
            {
              label: archived ? "Unarchive" : "Archive",
              onSelect: () => run(() => archiveSession(row.id, !archived)),
            },
            { label: "Export…", onSelect: doExport },
            {
              label: "Delete",
              confirm: "Delete this session?",
              onSelect: () => run(() => deleteSession(row.id)),
            },
          ]}
        />
      )}
    </li>
  );
}

function RenameInput({
  initial,
  onDone,
}: {
  initial: string;
  onDone: (title: string | undefined) => void;
}) {
  const [text, setText] = useState(initial);
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    ref.current?.focus();
    ref.current?.select();
  }, []);
  const onKey = (e: KeyboardEvent<HTMLInputElement>) => {
    e.stopPropagation();
    if (e.key === "Enter") onDone(text.trim() === "" ? undefined : text.trim());
    else if (e.key === "Escape") onDone(undefined);
  };
  return (
    <input
      ref={ref}
      className="input min-w-0 grow px-1 py-0 text-sm"
      aria-label="New title"
      value={text}
      onChange={(e) => setText(e.target.value)}
      onKeyDown={onKey}
      onBlur={() => onDone(text.trim() === "" ? undefined : text.trim())}
      onClick={(e) => e.stopPropagation()}
    />
  );
}

interface MenuItem {
  label: string;
  /** A second click is needed, with this text, before it runs. */
  confirm?: string;
  onSelect: () => void;
}

/** A small popover menu at a point; closes on outside click or Esc. */
export function Menu({
  at,
  items,
  onClose,
}: {
  at: { x: number; y: number };
  items: MenuItem[];
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [confirming, setConfirming] = useState<string>();
  useEffect(() => {
    const onDown = (e: globalThis.MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) onClose();
    };
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    document.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey, true);
    ref.current?.querySelector("button")?.focus();
    return () => {
      document.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [onClose]);
  // Keep it inside the window.
  const style = {
    left: Math.min(at.x, window.innerWidth - 180),
    top: Math.min(at.y, window.innerHeight - 40 * items.length - 8),
  };
  return (
    <div
      ref={ref}
      role="menu"
      className="fixed z-40 min-w-40 rounded border border-border bg-panel py-1 text-sm shadow-lg"
      style={style}
      onClick={(e) => e.stopPropagation()}
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((item) => (
        <button
          key={item.label}
          type="button"
          role="menuitem"
          className="block w-full px-3 py-1 text-left hover:bg-bg"
          onClick={() => {
            if (item.confirm !== undefined && confirming !== item.label) {
              setConfirming(item.label);
              return;
            }
            item.onSelect();
          }}
        >
          {item.confirm !== undefined && confirming === item.label ? (
            <span className="text-bad">{item.confirm} Click again.</span>
          ) : (
            item.label
          )}
        </button>
      ))}
    </div>
  );
}
