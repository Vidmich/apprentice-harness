// The usage panel (task M01-13): the range → `stats.tokens` params, the
// CSV of the calls table (the CLI's `stats calls --csv` columns), the
// pretty body of a stored request, and the effects that read
// `stats.tokens`, `stats.calls` and `trace.get` into `stores/usage.ts`.

import { useStore } from "../store";
import { type UsageRange, useUsage } from "../stores/usage";
import type { CallSummary, RpcError, StatsCallsParams, StatsGroup, TokenBucket } from "./api";
import { RpcFailure, call, describe } from "./rpc";

/** Rows of the calls table per page. */
export const CALLS_PAGE = 50;
/** Rows the CSV export fetches at most (in pages of `EXPORT_PAGE`). */
export const EXPORT_MAX = 10_000;
export const EXPORT_PAGE = 1000;
/** Every breakdown the panel shows. */
export const GROUPS: StatsGroup[] = ["day", "model", "workspace", "session", "kind"];

// ------------------------------------------------------------------ pure

/** `YYYY-MM-DD` of a time in the local zone (the daemon reads bare dates there). */
export function localDate(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/**
 * The `since` / `until` of a range, as the daemon parses them: today is
 * the local date, the presets are ages, a custom range its dates (an
 * `until` date includes that whole day).
 */
export function rangeParams(
  range: UsageRange,
  now = new Date(),
): Pick<StatsCallsParams, "since" | "until"> {
  switch (range.preset) {
    case "today":
      return { since: localDate(now) };
    case "7d":
      return { since: "7d" };
    case "30d":
      return { since: "30d" };
    case "custom": {
      const out: Pick<StatsCallsParams, "since" | "until"> = {};
      if (range.since !== "") out.since = range.since;
      if (range.until !== "") out.until = range.until;
      return out;
    }
  }
}

/** What a range is called in the panel's header. */
export function rangeLabel(range: UsageRange): string {
  switch (range.preset) {
    case "today":
      return "today";
    case "7d":
      return "last 7 days";
    case "30d":
      return "last 30 days";
    case "custom": {
      const since = range.since === "" ? "the beginning" : range.since;
      const until = range.until === "" ? "now" : range.until;
      return `${since} → ${until}`;
    }
  }
}

/** Every token of a bucket: input, output, cache read and cache write. */
export function tokensOf(b: TokenBucket): number {
  return b.input + b.output + b.cache_read + b.cache_creation;
}

/** The columns of the export, in the CLI's order (`harness stats calls --csv`). */
export const CALL_CSV_COLUMNS = [
  "call_id",
  "started_at",
  "session_id",
  "session_title",
  "workspace_id",
  "agent_id",
  "kind",
  "model",
  "effort",
  "status",
  "stop_reason",
  "input",
  "output",
  "cache_read",
  "cache_creation",
  "cost_usd",
  "total_ms",
  "request_event_id",
] as const;

function csvField(s: string): string {
  return /[",\r\n]/.test(s) ? `"${s.replaceAll('"', '""')}"` : s;
}

/** RFC 4180 (`\r\n`, a header line), absent values empty, the cost at full precision. */
export function callsCsv(calls: readonly CallSummary[]): string {
  const lines = [CALL_CSV_COLUMNS.join(",")];
  for (const c of calls) {
    const fields = [
      c.call_id,
      c.started_at,
      c.session_id,
      c.session_title ?? "",
      c.workspace_id ?? "",
      c.agent_id,
      c.kind,
      c.model,
      c.effort ?? "",
      c.status,
      c.stop_reason ?? "",
      String(c.input),
      String(c.output),
      String(c.cache_read),
      String(c.cache_creation),
      c.cost_usd === undefined ? "" : String(c.cost_usd),
      c.total_ms === undefined ? "" : String(c.total_ms),
      c.request_event_id,
    ];
    lines.push(fields.map(csvField).join(","));
  }
  return `${lines.join("\r\n")}\r\n`;
}

/** The stored body, indented when it is JSON; as stored otherwise. */
export function prettyJson(text: string): string {
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
}

/** `calls-2026-09-13.csv`, the export's suggested name. */
export function exportName(range: UsageRange, now = new Date()): string {
  const tag =
    range.preset === "custom" ? `${range.since || "start"}_${range.until || "now"}` : range.preset;
  return `mentor-calls-${tag}-${localDate(now)}.csv`;
}

// --------------------------------------------------------------- effects

function toError(e: unknown): RpcError {
  return e instanceof RpcFailure ? e.error : { code: -32603, message: String(e) };
}

function scope(): Pick<StatsCallsParams, "since" | "until" | "workspace_id"> {
  const range = useUsage.getState().range;
  const params: Pick<StatsCallsParams, "since" | "until" | "workspace_id"> = rangeParams(range);
  const ws = useStore.getState().selectedWorkspace;
  if (ws !== null) params.workspace_id = ws;
  return params;
}

let statsSeq = 0;
let callsSeq = 0;

/** Re-reads the totals and breakdowns for the range and the selected workspace. */
export async function refreshStats(): Promise<void> {
  const usage = useUsage.getState();
  if (!useStore.getState().daemon.connected) return;
  const seq = ++statsSeq;
  usage.setLoading(true);
  try {
    const stats = await call("stats.tokens", { ...scope(), group_by: GROUPS });
    if (seq !== statsSeq) return;
    usage.setStats(stats);
    usage.setError(undefined);
  } catch (e) {
    if (seq !== statsSeq) return;
    usage.setError(describe(toError(e)));
  } finally {
    if (seq === statsSeq) usage.setLoading(false);
  }
}

/** Re-reads one page of the calls table. */
export async function refreshCalls(page = useUsage.getState().page): Promise<void> {
  const usage = useUsage.getState();
  if (!useStore.getState().daemon.connected) return;
  const seq = ++callsSeq;
  try {
    const got = await call("stats.calls", {
      ...scope(),
      limit: CALLS_PAGE,
      offset: page * CALLS_PAGE,
    });
    if (seq !== callsSeq) return;
    // Past the end (a deletion shrank the list): the last page.
    const last = Math.max(0, Math.ceil(got.total / CALLS_PAGE) - 1);
    if (page > last) {
      await refreshCalls(last);
      return;
    }
    usage.setCalls(got.calls, got.total, page);
  } catch (e) {
    if (seq !== callsSeq) return;
    usage.setError(describe(toError(e)));
  }
}

/** Both reads; the panel calls it on show, on a range change, after a run. */
export async function refreshUsage(): Promise<void> {
  await Promise.all([refreshStats(), refreshCalls()]);
}

/** Every call of the range (newest first), for the export. */
export async function allCalls(): Promise<CallSummary[]> {
  const rows: CallSummary[] = [];
  const params = scope();
  for (let offset = 0; offset < EXPORT_MAX; offset += EXPORT_PAGE) {
    const got = await call("stats.calls", { ...params, limit: EXPORT_PAGE, offset });
    rows.push(...got.calls);
    if (rows.length >= got.total || got.calls.length === 0) break;
  }
  return rows;
}

/** Opens the drawer on a call and reads its request body (`trace.get` with the blob). */
export async function viewRequest(c: CallSummary): Promise<void> {
  const usage = useUsage.getState();
  usage.openDrawer(c);
  try {
    const got = await call("trace.get", { event_id: c.request_event_id, include_blob: true });
    if (useUsage.getState().drawer?.call.call_id !== c.call_id) return;
    if (got.blob === undefined) {
      usage.setDrawerBody(undefined, "the request body is not stored (the blob was pruned)");
    } else {
      usage.setDrawerBody(prettyJson(got.blob), undefined);
    }
  } catch (e) {
    if (useUsage.getState().drawer?.call.call_id !== c.call_id) return;
    usage.setDrawerBody(undefined, describe(toError(e)));
  }
}
