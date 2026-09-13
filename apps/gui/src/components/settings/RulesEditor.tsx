import { useCallback, useEffect, useState } from "react";
import type { ConfigLayer, RuleEffect, RuleInfo, RuleSpec, ToolsRulesResult } from "../../lib/api";
import { RpcFailure, call, openPath } from "../../lib/rpc";
import RuleFields, { cleanMatch, describeMatch } from "../RuleFields";

const EFFECT_STYLE: Record<RuleEffect, string> = {
  allow: "text-ok",
  deny: "text-bad",
  ask: "text-warn",
};

/**
 * The permission rules in force for a workspace (its file, the user
 * file, the built-ins) from `tools.rules`, with a form that appends one
 * (`tools.allow` / `tools.deny`) and a remove per file rule
 * (`tools.remove`).
 */
export default function RulesEditor({ workspace }: { workspace: string | undefined }) {
  const [rules, setRules] = useState<ToolsRulesResult>();
  const [tools, setTools] = useState<string[]>([]);
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [draft, setDraft] = useState<RuleSpec>({ tool: "", effect: "allow", match: {} });
  const [layer, setLayer] = useState<ConfigLayer>(workspace === undefined ? "user" : "workspace");

  const load = useCallback(
    () =>
      call("tools.rules", workspace === undefined ? {} : { workspace }).then(
        (r) => {
          setRules(r);
          setError(undefined);
        },
        (e: unknown) => setError(e instanceof RpcFailure ? e.message : String(e)),
      ),
    [workspace],
  );

  useEffect(() => {
    void load();
    call("tools.list", {}).then(
      (r) => setTools(r.tools.map((t) => t.name)),
      () => setTools([]),
    );
  }, [load]);

  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    setError(undefined);
    try {
      await f();
      await load();
    } catch (e) {
      setError(e instanceof RpcFailure ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const add = () => {
    const tool = draft.tool.trim() === "" ? "*" : draft.tool.trim();
    const params =
      layer === "workspace" && workspace !== undefined
        ? { tool, match: cleanMatch(draft.match ?? {}), layer, workspace }
        : { tool, match: cleanMatch(draft.match ?? {}), layer: "user" as const };
    void run(async () => {
      await call(draft.effect === "deny" ? "tools.deny" : "tools.allow", params);
      setDraft({ tool: "", effect: draft.effect, match: {} });
    });
  };

  const remove = (r: RuleInfo) => {
    if (r.source === "builtin") return;
    const params =
      r.source === "workspace" && workspace !== undefined
        ? { layer: "workspace" as const, workspace, index: r.index }
        : { layer: "user" as const, index: r.index };
    void run(() => call("tools.remove", params));
  };

  return (
    <div className="flex flex-col gap-3">
      {rules?.files.map((f) => (
        <div key={f.source} className="flex items-center gap-2 text-xs">
          <span className="w-20 shrink-0 text-muted">{f.source} file</span>
          <span className="truncate font-mono" title={f.path}>
            {f.path}
          </span>
          <span className="shrink-0 text-muted">
            {f.error !== undefined
              ? "NOT IN FORCE"
              : f.exists
                ? `default = ${f.default ?? "ask"}`
                : "no file"}
          </span>
          {f.exists && (
            <button
              type="button"
              className="shrink-0 text-muted hover:text-fg"
              onClick={() => void openPath(f.path).catch((e: unknown) => setError(String(e)))}
            >
              open in editor
            </button>
          )}
          {f.error !== undefined && (
            <span className="truncate text-bad" title={f.error}>
              {f.error}
            </span>
          )}
        </div>
      ))}
      <div className="overflow-x-auto rounded border border-border">
        <table className="w-full text-xs">
          <thead className="bg-panel text-left text-muted">
            <tr>
              <th className="px-2 py-1 font-medium">rule</th>
              <th className="px-2 py-1 font-medium">effect</th>
              <th className="px-2 py-1 font-medium">tool</th>
              <th className="px-2 py-1 font-medium">match</th>
              <th className="px-2 py-1" />
            </tr>
          </thead>
          <tbody>
            {rules?.rules.map((r) => (
              <tr key={`${r.source}:${r.index}`} className="border-t border-border">
                <td className="px-2 py-1 font-mono whitespace-nowrap">
                  {r.name !== undefined ? `builtin:${r.name}` : `${r.source}:${r.index}`}
                  {r.line !== undefined && <span className="text-muted"> (line {r.line})</span>}
                </td>
                <td className={`px-2 py-1 ${EFFECT_STYLE[r.rule.effect]}`}>{r.rule.effect}</td>
                <td className="px-2 py-1 font-mono">{r.rule.tool}</td>
                <td className="px-2 py-1 font-mono">{describeMatch(r.rule.match ?? {})}</td>
                <td className="px-2 py-1 text-right">
                  {r.source !== "builtin" && (
                    <button
                      type="button"
                      className="text-muted hover:text-bad"
                      title="Remove this rule from its file"
                      aria-label={`Remove rule ${r.source}:${r.index}`}
                      disabled={busy}
                      onClick={() => remove(r)}
                    >
                      remove
                    </button>
                  )}
                </td>
              </tr>
            ))}
            {rules !== undefined && rules.rules.length === 0 && (
              <tr>
                <td className="px-2 py-2 text-muted" colSpan={5}>
                  no rules
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
      <div className="rounded border border-border p-2">
        <div className="mb-2 flex items-center gap-2 text-xs">
          <span className="text-muted">Add a rule:</span>
          <select
            className="input py-0 text-xs"
            aria-label="Effect"
            value={draft.effect}
            onChange={(e) => setDraft({ ...draft, effect: e.target.value as RuleEffect })}
          >
            <option value="allow">allow</option>
            <option value="deny">deny</option>
          </select>
          <span className="text-muted">to the</span>
          <select
            className="input py-0 text-xs"
            aria-label="Rules file"
            value={layer}
            onChange={(e) => setLayer(e.target.value as ConfigLayer)}
          >
            <option value="user">user file</option>
            {workspace !== undefined && <option value="workspace">workspace file</option>}
          </select>
        </div>
        <RuleFields rule={draft} onChange={setDraft} tools={tools} idPrefix="rule-add" />
        <div className="mt-2">
          <button type="button" className="btn" disabled={busy} onClick={add}>
            Add rule
          </button>
        </div>
      </div>
      {error !== undefined && (
        <p className="text-sm text-bad" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
