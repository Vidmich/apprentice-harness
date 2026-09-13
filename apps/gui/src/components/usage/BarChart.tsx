import type { TokenBucket } from "../../lib/api";
import { compactNum, thousands, usd } from "../../lib/format";
import { tokensOf } from "../../lib/usage";
import type { ChartMode } from "../../stores/usage";

interface Props {
  /** `by_day`, ascending. */
  days: TokenBucket[];
  mode: ChartMode;
}

const W = 720;
const H = 160;
const PAD = { top: 8, right: 8, bottom: 22, left: 48 };

/** `$1.84` or `1.2M` for the axis and the bar tips. */
function fmt(mode: ChartMode, v: number): string {
  return mode === "cost" ? usd(v) : compactNum(v);
}

/** A round upper bound for the axis: 1, 2, 5 × 10^n above the maximum. */
export function niceMax(max: number): number {
  if (max <= 0) return 1;
  const pow = 10 ** Math.floor(Math.log10(max));
  for (const m of [1, 2, 5, 10]) if (m * pow >= max) return m * pow;
  return 10 * pow;
}

/** One bar per day of the range (days without calls are drawn empty). */
export default function BarChart({ days, mode }: Props) {
  const value = (b: TokenBucket) => (mode === "cost" ? b.cost_usd : tokensOf(b));
  const bars = fillDays(days);
  const top = niceMax(Math.max(0, ...bars.map(value)));
  const innerW = W - PAD.left - PAD.right;
  const innerH = H - PAD.top - PAD.bottom;
  const slot = innerW / Math.max(1, bars.length);
  const barW = Math.max(2, Math.min(28, slot * 0.7));
  const labelEvery = Math.max(1, Math.ceil(bars.length / 10));
  const ticks = [0, 0.5, 1];

  return (
    <svg
      viewBox={`0 0 ${W} ${H}`}
      className="h-40 w-full"
      role="img"
      aria-label={`${mode === "cost" ? "Cost" : "Tokens"} by day`}
    >
      {ticks.map((t) => {
        const y = PAD.top + innerH * (1 - t);
        return (
          <g key={t}>
            <line
              x1={PAD.left}
              x2={W - PAD.right}
              y1={y}
              y2={y}
              className="stroke-border"
              strokeWidth={1}
            />
            <text x={PAD.left - 6} y={y + 4} textAnchor="end" className="fill-muted" fontSize={10}>
              {fmt(mode, top * t)}
            </text>
          </g>
        );
      })}
      {bars.map((b, i) => {
        const v = value(b);
        const h = top === 0 ? 0 : (v / top) * innerH;
        const x = PAD.left + slot * i + (slot - barW) / 2;
        const y = PAD.top + innerH - h;
        const tip =
          v === 0
            ? `${b.key}: no calls`
            : `${b.key}: ${usd(b.cost_usd)} · ${thousands(tokensOf(b))} tokens · ${b.calls} calls`;
        return (
          <g key={b.key}>
            <title>{tip}</title>
            <rect
              x={x}
              y={y}
              width={barW}
              height={h}
              rx={2}
              className={v === 0 ? "fill-border" : "fill-accent"}
              opacity={v === 0 ? 0.5 : 0.85}
            />
            {v === 0 && (
              <rect
                x={x}
                y={PAD.top + innerH - 1}
                width={barW}
                height={1}
                className="fill-border"
              />
            )}
            {i % labelEvery === 0 && (
              <text
                x={x + barW / 2}
                y={H - 6}
                textAnchor="middle"
                className="fill-muted"
                fontSize={10}
              >
                {b.key?.slice(5)}
              </text>
            )}
          </g>
        );
      })}
    </svg>
  );
}

/** The days between the first and the last bucket, the gaps as empty buckets. */
export function fillDays(days: TokenBucket[]): TokenBucket[] {
  const first = days[0]?.key;
  const last = days[days.length - 1]?.key;
  if (first === undefined || last === undefined) return [];
  const byKey = new Map(days.map((d) => [d.key, d]));
  const out: TokenBucket[] = [];
  const d = new Date(`${first}T00:00:00Z`);
  const end = new Date(`${last}T00:00:00Z`);
  if (Number.isNaN(d.getTime()) || Number.isNaN(end.getTime())) return days;
  for (let n = 0; d <= end && n < 400; n++, d.setUTCDate(d.getUTCDate() + 1)) {
    const key = d.toISOString().slice(0, 10);
    out.push(
      byKey.get(key) ?? {
        key,
        calls: 0,
        input: 0,
        output: 0,
        cache_read: 0,
        cache_creation: 0,
        cost_usd: 0,
        unpriced_calls: 0,
      },
    );
  }
  return out;
}
