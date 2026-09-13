import type { Risk, RuleMatch, RuleSpec } from "../lib/api";

export const RISKS: readonly Risk[] = ["read_only", "write", "execute", "network"];

/** `allow shell [command_prefix="cargo test"]`, the CLI's rendering. */
export function describeRule(rule: RuleSpec): string {
  const m = describeMatch(rule.match ?? {});
  return m === "" ? `${rule.effect} ${rule.tool}` : `${rule.effect} ${rule.tool} [${m}]`;
}

export function describeMatch(m: RuleMatch): string {
  const parts: string[] = [];
  if (m.risk !== undefined) parts.push(`risk=${m.risk}`);
  if (m.path !== undefined) parts.push(`path=${m.path}`);
  if (m.command_prefix !== undefined)
    parts.push(`command_prefix=${JSON.stringify(m.command_prefix)}`);
  if (m.command_regex !== undefined) parts.push(`command_regex=${JSON.stringify(m.command_regex)}`);
  if (m.outside_workspace !== undefined) parts.push(`outside_workspace=${m.outside_workspace}`);
  return parts.join(", ");
}

/** A match without its empty fields (what the daemon's `deny_unknown_fields` shape wants). */
export function cleanMatch(m: RuleMatch): RuleMatch {
  const out: RuleMatch = {};
  if (m.path !== undefined && m.path.trim() !== "") out.path = m.path.trim();
  if (m.command_prefix !== undefined && m.command_prefix.trim() !== "") {
    out.command_prefix = m.command_prefix.trim();
  }
  if (m.command_regex !== undefined && m.command_regex.trim() !== "") {
    out.command_regex = m.command_regex.trim();
  }
  if (m.outside_workspace !== undefined) out.outside_workspace = m.outside_workspace;
  if (m.risk !== undefined) out.risk = m.risk;
  return out;
}

/**
 * The editable parts of a rule: the tool and the match conditions
 * (glob, command prefix or regex, inside/outside the workspace, risk).
 */
export default function RuleFields({
  rule,
  onChange,
  tools,
  idPrefix,
  disabled,
}: {
  rule: RuleSpec;
  onChange: (rule: RuleSpec) => void;
  /** Tool names to offer; free text otherwise. */
  tools?: string[];
  idPrefix: string;
  disabled?: boolean;
}) {
  const m = rule.match ?? {};
  const text = (key: "path" | "command_prefix" | "command_regex", value: string) => {
    const next = { ...m };
    if (value === "") delete next[key];
    else next[key] = value;
    onChange({ ...rule, match: next });
  };
  const setOutside = (value: string) => {
    const next = { ...m };
    if (value === "any") delete next.outside_workspace;
    else next.outside_workspace = value === "only";
    onChange({ ...rule, match: next });
  };
  const setRisk = (value: string) => {
    const next = { ...m };
    if (value === "") delete next.risk;
    else next.risk = value as Risk;
    onChange({ ...rule, match: next });
  };
  const outside = m.outside_workspace === undefined ? "any" : m.outside_workspace ? "only" : "none";
  const field = "flex items-center gap-2 text-xs";
  const label = "w-28 shrink-0 text-muted";
  return (
    <div className="flex flex-col gap-1">
      <label className={field}>
        <span className={label}>tool</span>
        <input
          id={`${idPrefix}-tool`}
          className="input grow py-0 font-mono"
          list={tools === undefined ? undefined : `${idPrefix}-tools`}
          value={rule.tool}
          disabled={disabled}
          onChange={(e) => onChange({ ...rule, tool: e.target.value })}
          placeholder="* for every tool"
        />
        {tools !== undefined && (
          <datalist id={`${idPrefix}-tools`}>
            <option value="*" />
            {tools.map((t) => (
              <option key={t} value={t} />
            ))}
          </datalist>
        )}
      </label>
      <label className={field}>
        <span className={label}>path glob</span>
        <input
          className="input grow py-0 font-mono"
          value={m.path ?? ""}
          disabled={disabled}
          onChange={(e) => text("path", e.target.value)}
          placeholder="src/** (root-relative)"
        />
      </label>
      <label className={field}>
        <span className={label}>command prefix</span>
        <input
          className="input grow py-0 font-mono"
          value={m.command_prefix ?? ""}
          disabled={disabled}
          onChange={(e) => text("command_prefix", e.target.value)}
          placeholder="cargo test"
        />
      </label>
      <label className={field}>
        <span className={label}>command regex</span>
        <input
          className="input grow py-0 font-mono"
          value={m.command_regex ?? ""}
          disabled={disabled}
          onChange={(e) => text("command_regex", e.target.value)}
          placeholder="\\bgit\\s+push\\b"
        />
      </label>
      <label className={field}>
        <span className={label}>paths</span>
        <select
          className="input grow py-0"
          value={outside}
          disabled={disabled}
          onChange={(e) => setOutside(e.target.value)}
        >
          <option value="any">anywhere</option>
          <option value="none">inside the workspace only</option>
          <option value="only">outside the workspace only</option>
        </select>
      </label>
      <label className={field}>
        <span className={label}>risk</span>
        <select
          className="input grow py-0"
          value={m.risk ?? ""}
          disabled={disabled}
          onChange={(e) => setRisk(e.target.value)}
        >
          <option value="">any</option>
          {RISKS.map((r) => (
            <option key={r} value={r}>
              {r}
            </option>
          ))}
        </select>
      </label>
    </div>
  );
}
