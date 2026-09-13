import { useEffect } from "react";
import { bytes, thousands, usd } from "../../lib/format";
import { saveTextFile } from "../../lib/ui";
import { useUsage } from "../../stores/usage";
import CopyButton from "../chat/CopyButton";

/**
 * The exact `mentor.request` body behind a call (the trace blob), as
 * stored, indented when it is JSON. `Esc` closes it.
 */
export default function RequestDrawer() {
  const drawer = useUsage((s) => s.drawer);
  const close = useUsage((s) => s.closeDrawer);

  useEffect(() => {
    if (drawer === undefined) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        close();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [drawer, close]);

  if (drawer === undefined) return null;
  const c = drawer.call;
  const facts = [
    `${c.kind} · ${c.model}${c.effort === undefined ? "" : ` (${c.effort})`}`,
    `${c.status}${c.stop_reason === undefined ? "" : ` / ${c.stop_reason}`}`,
    `in ${thousands(c.input)} · out ${thousands(c.output)} · cache read ${thousands(c.cache_read)} · cache write ${thousands(c.cache_creation)}`,
    c.cost_usd === undefined ? "cost unknown" : usd(c.cost_usd),
  ];

  return (
    <aside
      className="absolute inset-y-0 right-0 z-20 flex w-[min(48rem,90%)] flex-col border-l border-border bg-bg shadow-xl"
      role="dialog"
      aria-label="Mentor request"
      data-testid="request-drawer"
    >
      <header className="flex items-center gap-2 border-b border-border px-3 py-2 text-sm">
        <span className="font-medium">Request</span>
        <span className="truncate font-mono text-xs text-muted" title={c.request_event_id}>
          {c.call_id} · event {c.request_event_id}
        </span>
        <span className="grow" />
        {drawer.body !== undefined && (
          <>
            <span className="text-xs text-muted">{bytes(drawer.body.length)}</span>
            <CopyButton text={drawer.body} label="copy" />
            <button
              type="button"
              className="btn py-0 text-xs"
              onClick={() => void saveTextFile(`${c.call_id}-request.json`, drawer.body ?? "")}
            >
              save…
            </button>
          </>
        )}
        <button type="button" className="btn py-0 text-xs" onClick={close} aria-label="Close">
          ✕
        </button>
      </header>
      <p className="border-b border-border px-3 py-1 text-xs text-muted">{facts.join(" · ")}</p>
      <div className="min-h-0 grow overflow-auto">
        {drawer.error !== undefined && (
          <p className="px-3 py-2 text-sm text-bad" role="alert">
            {drawer.error}
          </p>
        )}
        {drawer.body === undefined && drawer.error === undefined && (
          <p className="px-3 py-2 text-sm text-muted">loading…</p>
        )}
        {drawer.body !== undefined && (
          <pre className="px-3 py-2 font-mono text-xs leading-5 whitespace-pre-wrap break-all">
            {drawer.body}
          </pre>
        )}
      </div>
    </aside>
  );
}
