import type { TokenBucket } from "../../lib/api";
import { thousands, usd } from "../../lib/format";

interface Props {
  title: string;
  /** The first column's header. */
  keyLabel: string;
  rows: TokenBucket[];
  /** What a row's first column shows; default the label, else the key. */
  name?: (b: TokenBucket) => string;
  /** When given, the first column is a link. */
  onRow?: (b: TokenBucket) => void;
  /** Shown when there are no rows. */
  empty?: string;
}

/** The numeric columns of every breakdown table. */
export const HEADERS = ["calls", "in", "out", "cache rd", "cache wr", "cost"];

/** One breakdown of `stats.tokens` as a table (the CLI's columns). */
export default function BucketTable({ title, keyLabel, rows, name, onRow, empty }: Props) {
  const label = name ?? ((b: TokenBucket) => b.label ?? b.key ?? "(none)");
  return (
    <section className="min-w-0">
      <h3 className="mb-1 text-xs font-medium tracking-wide text-muted uppercase">{title}</h3>
      <div className="overflow-x-auto rounded border border-border">
        <table className="w-full text-xs">
          <thead className="bg-panel text-muted">
            <tr>
              <th className="px-2 py-1 text-left font-normal">{keyLabel}</th>
              {HEADERS.map((h) => (
                <th key={h} className="px-2 py-1 text-right font-normal whitespace-nowrap">
                  {h}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.length === 0 && (
              <tr>
                <td className="px-2 py-1 text-muted" colSpan={HEADERS.length + 1}>
                  {empty ?? "(none)"}
                </td>
              </tr>
            )}
            {rows.map((b) => (
              <tr key={b.key ?? ""} className="border-t border-border">
                <td className="max-w-[16rem] truncate px-2 py-1" title={b.key ?? ""}>
                  {onRow === undefined ? (
                    label(b)
                  ) : (
                    <button
                      type="button"
                      className="max-w-full truncate text-left text-accent hover:underline"
                      onClick={() => onRow(b)}
                    >
                      {label(b)}
                    </button>
                  )}
                </td>
                <Num v={b.calls} />
                <Num v={b.input} />
                <Num v={b.output} />
                <Num v={b.cache_read} />
                <Num v={b.cache_creation} />
                <td className="px-2 py-1 text-right font-mono whitespace-nowrap">
                  {usd(b.cost_usd)}
                  {b.unpriced_calls > 0 && (
                    <span
                      className="text-warn"
                      title={`${b.unpriced_calls} calls without a [pricing] entry are not in the cost`}
                    >
                      *
                    </span>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}

function Num({ v }: { v: number }) {
  return <td className="px-2 py-1 text-right font-mono whitespace-nowrap">{thousands(v)}</td>;
}
