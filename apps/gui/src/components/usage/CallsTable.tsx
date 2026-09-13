import type { CallSummary } from "../../lib/api";
import { duration, thousands, usd } from "../../lib/format";
import { showSession } from "../../lib/sessions";
import { CALLS_PAGE, refreshCalls, viewRequest } from "../../lib/usage";
import { useJump } from "../../stores/jump";
import { useUsage } from "../../stores/usage";

/** `Sep 12, 10:04` of an RFC 3339 time, local. */
function when(ts: string): string {
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return ts;
  return d.toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** Opens the call's session in a chat, scrolled to its agent's turn. */
export function openCall(c: CallSummary): void {
  useJump.getState().set(c.session_id, c.agent_id);
  showSession(c.session_id, undefined);
}

const STATUS_CLASS: Record<CallSummary["status"], string> = {
  ok: "text-ok",
  running: "text-accent",
  cancelled: "text-muted",
  error: "text-bad",
};

/** The individual calls of the range, newest first, one page at a time. */
export default function CallsTable() {
  const calls = useUsage((s) => s.calls);
  const total = useUsage((s) => s.total);
  const page = useUsage((s) => s.page);
  const pages = Math.max(1, Math.ceil(total / CALLS_PAGE));
  const first = total === 0 ? 0 : page * CALLS_PAGE + 1;
  const last = Math.min(total, (page + 1) * CALLS_PAGE);

  return (
    <section className="min-w-0">
      <div className="mb-1 flex items-center gap-2">
        <h3 className="text-xs font-medium tracking-wide text-muted uppercase">Calls</h3>
        <span className="grow" />
        <span className="text-xs text-muted">
          {total === 0 ? "none" : `${thousands(first)}–${thousands(last)} of ${thousands(total)}`}
        </span>
        <button
          type="button"
          className="btn px-2 py-0 text-xs"
          disabled={page === 0}
          onClick={() => void refreshCalls(page - 1)}
          aria-label="Newer calls"
        >
          ‹
        </button>
        <button
          type="button"
          className="btn px-2 py-0 text-xs"
          disabled={page + 1 >= pages}
          onClick={() => void refreshCalls(page + 1)}
          aria-label="Older calls"
        >
          ›
        </button>
      </div>
      <div className="overflow-x-auto rounded border border-border">
        <table className="w-full text-xs" data-testid="calls-table">
          <thead className="bg-panel text-muted">
            <tr>
              {[
                "when",
                "session",
                "kind",
                "model",
                "status",
                "in",
                "out",
                "cache rd",
                "cost",
                "time",
                "",
              ].map((h, i) => (
                <th
                  key={h === "" ? "actions" : h}
                  className={`px-2 py-1 font-normal whitespace-nowrap ${i >= 5 && i <= 9 ? "text-right" : "text-left"}`}
                >
                  {h}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {calls.length === 0 && (
              <tr>
                <td className="px-2 py-1 text-muted" colSpan={11}>
                  no mentor calls in this range
                </td>
              </tr>
            )}
            {calls.map((c) => (
              <tr key={c.call_id} className="border-t border-border">
                <td className="px-2 py-1 whitespace-nowrap" title={c.started_at}>
                  {when(c.started_at)}
                </td>
                <td className="max-w-[14rem] truncate px-2 py-1" title={c.session_id}>
                  <button
                    type="button"
                    className="max-w-full truncate text-left text-accent hover:underline"
                    onClick={() => openCall(c)}
                    title="Open the session at this turn"
                  >
                    {c.session_title ?? c.session_id}
                  </button>
                </td>
                <td className="px-2 py-1">
                  <span
                    className={
                      c.kind === "title"
                        ? "rounded border border-border px-1 text-[10px] text-muted"
                        : ""
                    }
                  >
                    {c.kind}
                  </span>
                </td>
                <td className="px-2 py-1 font-mono whitespace-nowrap" title={c.effort ?? ""}>
                  {c.model}
                </td>
                <td className={`px-2 py-1 ${STATUS_CLASS[c.status]}`} title={c.stop_reason ?? ""}>
                  {c.status}
                </td>
                <td className="px-2 py-1 text-right font-mono">{thousands(c.input)}</td>
                <td className="px-2 py-1 text-right font-mono">{thousands(c.output)}</td>
                <td className="px-2 py-1 text-right font-mono">{thousands(c.cache_read)}</td>
                <td className="px-2 py-1 text-right font-mono whitespace-nowrap">
                  {c.cost_usd === undefined ? "–" : usd(c.cost_usd)}
                </td>
                <td className="px-2 py-1 text-right font-mono whitespace-nowrap">
                  {c.total_ms === undefined ? "–" : duration(c.total_ms)}
                </td>
                <td className="px-2 py-1 text-right whitespace-nowrap">
                  <button
                    type="button"
                    className="text-accent hover:underline"
                    onClick={() => void viewRequest(c)}
                  >
                    View request
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}
