import { useEffect, useState } from "react";
import type { TokenBucket } from "../../lib/api";
import { thousands, usd } from "../../lib/format";
import { showSession } from "../../lib/sessions";
import { saveTextFile } from "../../lib/ui";
import { allCalls, callsCsv, exportName, rangeLabel, refreshUsage } from "../../lib/usage";
import { selectedWorkspace, useStore } from "../../store";
import { type RangePreset, type UsageRange, useUsage } from "../../stores/usage";
import BarChart from "./BarChart";
import BucketTable from "./BucketTable";
import CallsTable from "./CallsTable";
import RequestDrawer from "./RequestDrawer";

const PRESETS: { id: RangePreset; label: string }[] = [
  { id: "today", label: "Today" },
  { id: "7d", label: "7 days" },
  { id: "30d", label: "30 days" },
  { id: "custom", label: "Custom" },
];

/**
 * The usage panel (task M01-13): the mentor's calls, tokens and cost
 * over a range for the selected workspace (or all), by day, model,
 * workspace, session and kind, and the calls behind them.
 */
export default function Usage() {
  const range = useUsage((s) => s.range);
  const setRange = useUsage((s) => s.setRange);
  const chart = useUsage((s) => s.chart);
  const setChart = useUsage((s) => s.setChart);
  const stats = useUsage((s) => s.stats);
  const loading = useUsage((s) => s.loading);
  const error = useUsage((s) => s.error);
  const workspace = useStore(selectedWorkspace);
  const workspaceId = useStore((s) => s.selectedWorkspace);
  const connected = useStore((s) => s.daemon.connected);
  const [exporting, setExporting] = useState<string | undefined>(undefined);

  // Read on show and whenever the scope changes.
  useEffect(() => {
    if (connected) void refreshUsage();
  }, [connected, range, workspaceId]);

  const pick = (patch: Partial<UsageRange>) => setRange({ ...range, ...patch });

  const exportCsv = async () => {
    setExporting("exporting…");
    try {
      const rows = await allCalls();
      const path = await saveTextFile(exportName(range), callsCsv(rows));
      setExporting(path === undefined ? undefined : `${rows.length} rows saved`);
    } catch (e) {
      setExporting(`export failed: ${String(e)}`);
    }
  };

  const openBucketSession = (b: TokenBucket) => {
    if (b.key !== undefined) showSession(b.key, undefined);
  };
  const t = stats?.totals;

  return (
    <div className="relative flex h-full flex-col">
      <header className="flex flex-wrap items-center gap-2 border-b border-border px-4 py-2 text-sm">
        <span className="font-medium">Usage</span>
        <span className="text-xs text-muted" title="Pick a workspace in the sidebar to narrow it">
          {workspace === undefined ? "all workspaces" : workspace.name} · {rangeLabel(range)}
          {stats !== undefined && ` · days in ${stats.tz}`}
        </span>
        <span className="grow" />
        <div className="flex items-center gap-1" role="group" aria-label="Range">
          {PRESETS.map((p) => (
            <button
              key={p.id}
              type="button"
              className={`btn px-2 py-0 text-xs ${range.preset === p.id ? "border-accent" : ""}`}
              aria-pressed={range.preset === p.id}
              onClick={() => pick({ preset: p.id })}
            >
              {p.label}
            </button>
          ))}
        </div>
        {range.preset === "custom" && (
          <div className="flex items-center gap-1 text-xs">
            <input
              type="date"
              className="input py-0 text-xs"
              value={range.since}
              onChange={(e) => pick({ since: e.target.value })}
              aria-label="Since"
            />
            <span className="text-muted">→</span>
            <input
              type="date"
              className="input py-0 text-xs"
              value={range.until}
              onChange={(e) => pick({ until: e.target.value })}
              aria-label="Until"
            />
          </div>
        )}
        <button
          type="button"
          className="btn px-2 py-0 text-xs"
          onClick={() => void refreshUsage()}
          disabled={loading}
          title="Re-read"
          aria-label="Refresh"
        >
          ↻
        </button>
        <button
          type="button"
          className="btn py-0 text-xs"
          onClick={() => void exportCsv()}
          disabled={!connected || exporting === "exporting…"}
          title="Every call of the range as CSV"
        >
          Export CSV
        </button>
        {exporting !== undefined && exporting !== "exporting…" && (
          <span className="text-xs text-muted">{exporting}</span>
        )}
      </header>
      {error !== undefined && (
        <p className="border-b border-border px-4 py-1 text-sm text-bad" role="alert">
          {error}
        </p>
      )}
      <div className="min-h-0 grow overflow-y-auto px-4 py-3">
        {stats === undefined ? (
          <p className="mt-16 text-center text-sm text-muted">
            {connected ? "loading…" : "Waiting for the daemon…"}
          </p>
        ) : (
          <div className="flex flex-col gap-4">
            <div className="grid grid-cols-2 gap-2 md:grid-cols-5" data-testid="usage-tiles">
              <Tile label="calls" value={thousands(t!.calls)} />
              <Tile label="input" value={thousands(t!.input)} />
              <Tile label="output" value={thousands(t!.output)} />
              <Tile
                label="cache read"
                value={thousands(t!.cache_read)}
                hint={
                  t!.cache_creation > 0 ? `cache write ${thousands(t!.cache_creation)}` : undefined
                }
              />
              <Tile
                label="cost"
                value={usd(t!.cost_usd)}
                hint={
                  t!.unpriced_calls > 0
                    ? `${t!.unpriced_calls} unpriced calls not included`
                    : undefined
                }
              />
            </div>
            <section>
              <div className="mb-1 flex items-center gap-2">
                <h3 className="text-xs font-medium tracking-wide text-muted uppercase">By day</h3>
                <span className="grow" />
                <div className="flex gap-1" role="group" aria-label="Chart shows">
                  {(["cost", "tokens"] as const).map((m) => (
                    <button
                      key={m}
                      type="button"
                      className={`btn px-2 py-0 text-xs ${chart === m ? "border-accent" : ""}`}
                      aria-pressed={chart === m}
                      onClick={() => setChart(m)}
                    >
                      {m}
                    </button>
                  ))}
                </div>
              </div>
              {stats.by_day.length === 0 ? (
                <p className="rounded border border-border px-2 py-6 text-center text-xs text-muted">
                  no mentor calls in this range
                </p>
              ) : (
                <div className="rounded border border-border px-2 py-1">
                  <BarChart days={stats.by_day} mode={chart} />
                </div>
              )}
            </section>
            <div className="grid gap-4 lg:grid-cols-2">
              <BucketTable title="By model" keyLabel="model" rows={stats.by_model} />
              <BucketTable
                title="By kind"
                keyLabel="kind"
                rows={stats.by_kind}
                name={(b) => (b.key === "title" ? "title (naming sessions)" : (b.key ?? ""))}
              />
              <BucketTable
                title="By workspace"
                keyLabel="workspace"
                rows={stats.by_workspace}
                name={(b) => b.label ?? (b.key === "" ? "(no workspace)" : (b.key ?? ""))}
              />
              <BucketTable
                title="By session"
                keyLabel="session"
                rows={stats.by_session}
                onRow={openBucketSession}
                empty={workspaceId === null ? "(none)" : "(none in this workspace)"}
              />
            </div>
            <CallsTable />
          </div>
        )}
      </div>
      <RequestDrawer />
    </div>
  );
}

function Tile({ label, value, hint }: { label: string; value: string; hint?: string | undefined }) {
  return (
    <div className="rounded border border-border bg-panel px-3 py-2">
      <div className="text-[11px] tracking-wide text-muted uppercase">{label}</div>
      <div className="font-mono text-lg">{value}</div>
      {hint !== undefined && <div className="text-[11px] text-muted">{hint}</div>}
    </div>
  );
}
