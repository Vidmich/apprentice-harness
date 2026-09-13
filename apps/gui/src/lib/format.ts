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
