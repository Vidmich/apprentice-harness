// Number and usage formatting shared by the chat view (CLI parity for the
// usage line).

import type { Usage } from "./api";

export const ZERO_USAGE: Usage = {
  input_tokens: 0,
  output_tokens: 0,
  cache_read_input_tokens: 0,
  cache_creation_input_tokens: 0,
};

export function addUsage(a: Usage, b: Usage): Usage {
  return {
    input_tokens: a.input_tokens + b.input_tokens,
    output_tokens: a.output_tokens + b.output_tokens,
    cache_read_input_tokens: a.cache_read_input_tokens + b.cache_read_input_tokens,
    cache_creation_input_tokens: a.cache_creation_input_tokens + b.cache_creation_input_tokens,
  };
}

export function thousands(n: number): string {
  return n.toLocaleString("en-US");
}

export function usd(v: number): string {
  return v !== 0 && Math.abs(v) < 1 ? `$${v.toFixed(4)}` : `$${v.toFixed(2)}`;
}

/** `182k`, `1.2M`, `950` — the session header's compact counts. */
export function compactNum(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${Math.round(n / 1000)}k`;
  return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, "")}M`;
}

/** `in 182k · out 21k · cached 610k · $1.84 · 14 calls` (the session header). */
export function sessionTotals(usage: Usage, costUsd: number | undefined, calls: number): string {
  let line = `in ${compactNum(usage.input_tokens)} · out ${compactNum(usage.output_tokens)} · cached ${compactNum(usage.cache_read_input_tokens)}`;
  if (costUsd !== undefined) line += ` · ${usd(costUsd)}`;
  line += ` · ${calls} ${calls === 1 ? "call" : "calls"}`;
  return line;
}

/** `1.2s`, `48s`, `3m 05s`. */
export function duration(ms: number): string {
  const s = ms / 1000;
  if (s < 10) return `${s.toFixed(1)}s`;
  if (s < 60) return `${Math.round(s)}s`;
  const m = Math.floor(s / 60);
  return `${m}m ${String(Math.round(s - m * 60)).padStart(2, "0")}s`;
}

/** `in 1,204 · out 310 · cache read 0 · $0.0138 · 4.2s` (CLI parity). */
export function usageLine(usage: Usage, costUsd: number | undefined, elapsedMs: number): string {
  let line = `in ${thousands(usage.input_tokens)} · out ${thousands(usage.output_tokens)} · cache read ${thousands(usage.cache_read_input_tokens)}`;
  if (usage.cache_creation_input_tokens > 0) {
    line += ` · cache write ${thousands(usage.cache_creation_input_tokens)}`;
  }
  if (costUsd !== undefined) line += ` · ${usd(costUsd)}`;
  line += ` · ${duration(elapsedMs)}`;
  return line;
}

/** `1.2 KB`, `5.0 MB`. */
export function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

/** One-line JSON cut to `max` characters, as in the CLI's activity log. */
export function compact(v: unknown, max = 120): string {
  const s = JSON.stringify(v) ?? "null";
  return s.length > max ? `${s.slice(0, max - 3)}...` : s;
}

/** `HH:MM` of an RFC 3339 time (local), or the input when unparseable. */
export function clock(ts: string | number): string {
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return String(ts);
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** `now`, `4m`, `3h`, `2d`, else the date — for the sessions list. */
export function ago(ts: string | number, now = Date.now()): string {
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return String(ts);
  const s = Math.max(0, Math.round((now - d.getTime()) / 1000));
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86_400) return `${Math.floor(s / 3600)}h`;
  if (s < 7 * 86_400) return `${Math.floor(s / 86_400)}d`;
  return d.toLocaleDateString([], { month: "short", day: "numeric" });
}

export type DayGroup = "Today" | "Yesterday" | "Earlier";

/** Which of the sidebar's groups a time falls in (local days). */
export function dayGroup(ts: string, now = new Date()): DayGroup {
  const d = new Date(ts);
  if (Number.isNaN(d.getTime())) return "Earlier";
  const day = (x: Date) => Math.floor((x.getTime() - x.getTimezoneOffset() * 60_000) / 86_400_000);
  const diff = day(now) - day(d);
  if (diff <= 0) return "Today";
  if (diff === 1) return "Yesterday";
  return "Earlier";
}

/** `1h 02m` for an uptime in seconds. */
export function uptime(s: number): string {
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}h ${String(m - h * 60).padStart(2, "0")}m`;
  return `${Math.floor(h / 24)}d ${h - Math.floor(h / 24) * 24}h`;
}
