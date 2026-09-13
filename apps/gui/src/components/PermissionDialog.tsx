import { type KeyboardEvent, useEffect, useMemo, useRef, useState } from "react";
import type { PermissionAnswer, RuleEffect, RuleSpec } from "../lib/api";
import { respondPermission } from "../lib/chat";
import { type PendingPermission, countdown, secondsLeft } from "../lib/permissions";
import { RpcFailure } from "../lib/rpc";
import { usePermissions } from "../stores/permissions";
import RuleFields, { cleanMatch, describeRule } from "./RuleFields";

const RISK_STYLE: Record<string, string> = {
  read_only: "bg-ok/20 text-ok",
  write: "bg-warn/20 text-warn",
  execute: "bg-bad/20 text-bad",
  network: "bg-accent/20 text-accent",
};

interface Choice {
  answer: PermissionAnswer;
  label: string;
  key: string;
  /** Writes a rule with this effect. */
  effect?: RuleEffect;
  primary?: boolean;
}

const CHOICES: Choice[] = [
  { answer: "allow_once", label: "Allow once", key: "a", primary: true },
  { answer: "allow_session", label: "Allow for session", key: "s" },
  { answer: "allow_workspace", label: "Allow in workspace", key: "w", effect: "allow" },
  { answer: "allow_always", label: "Always allow", key: "A", effect: "allow" },
  { answer: "deny_once", label: "Deny", key: "d" },
  { answer: "deny_always", label: "Deny always", key: "D", effect: "deny" },
];

/**
 * A `permission.request` of the session in front: what would run, the
 * rule a lasting answer writes (editable), the choices (the CLI's
 * letters work as keys), and the daemon's countdown.
 */
export default function PermissionDialog({
  request,
  hasWorkspace,
}: {
  request: PendingPermission;
  /** `Allow in workspace` needs one. */
  hasWorkspace: boolean;
}) {
  const [now, setNow] = useState(() => Date.now());
  const [ruleIndex, setRuleIndex] = useState(0);
  const [edited, setEdited] = useState<RuleSpec>();
  const [showInput, setShowInput] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const expire = usePermissions((s) => s.expire);
  const box = useRef<HTMLDivElement>(null);

  // Keyboard users land on the dialog itself; the keys below answer.
  useEffect(() => {
    box.current?.focus();
  }, []);

  // The countdown, and the store forgets requests the daemon timed out.
  useEffect(() => {
    const t = setInterval(() => {
      const tick = Date.now();
      setNow(tick);
      expire(tick);
    }, 1000);
    return () => clearInterval(t);
  }, [expire]);

  const suggested = request.suggestedRules;
  const base = useMemo<RuleSpec>(
    () => suggested[ruleIndex] ?? { tool: request.tool, effect: "allow" },
    [suggested, ruleIndex, request.tool],
  );
  const rule = edited ?? base;

  const answer = async (choice: Choice) => {
    if (busy) return;
    setBusy(true);
    setError(undefined);
    try {
      const spec: RuleSpec | undefined =
        choice.effect === undefined
          ? undefined
          : {
              tool: rule.tool.trim() === "" ? "*" : rule.tool.trim(),
              effect: choice.effect,
              match: cleanMatch(rule.match ?? {}),
            };
      await respondPermission(request.requestId, choice.answer, spec);
    } catch (e) {
      setError(e instanceof RpcFailure ? e.message : String(e));
      setBusy(false);
    }
  };

  const onKey = (e: KeyboardEvent) => {
    // Typing in the rule editor must not answer.
    if ((e.target as HTMLElement).tagName === "INPUT") return;
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const choice = CHOICES.find((c) => c.key === e.key);
    if (choice !== undefined) {
      e.preventDefault();
      e.stopPropagation();
      if (!(choice.answer === "allow_workspace" && !hasWorkspace)) void answer(choice);
    } else if (e.key === "Escape") {
      // Esc cancels the run elsewhere; here it does nothing.
      e.stopPropagation();
    }
  };

  const left = secondsLeft(request, now);
  const titleId = `perm-${request.requestId}-title`;
  return (
    <div
      className="absolute inset-0 z-30 flex items-start justify-center bg-bg/60 p-6 pt-16"
      onKeyDown={onKey}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        ref={box}
        className="flex max-h-full w-full max-w-2xl flex-col gap-3 overflow-y-auto rounded-lg border border-border bg-panel p-4 shadow-xl outline-none"
      >
        <div className="flex items-center gap-2">
          <h2 id={titleId} className="text-base font-semibold">
            Permission for <span className="font-mono">{request.tool}</span>
          </h2>
          <span
            className={`rounded px-1.5 py-0.5 text-[11px] font-medium ${RISK_STYLE[request.risk] ?? ""}`}
          >
            {request.risk.replace("_", " ")}
          </span>
          <span className="grow" />
          {Number.isFinite(left) && (
            <span className="text-xs text-muted" title="denied without an answer">
              denied in {countdown(left)}
            </span>
          )}
        </div>
        {request.description !== "" && <p className="text-sm">{request.description}</p>}
        {request.command !== undefined && (
          <pre className="max-h-40 overflow-auto rounded border border-border bg-bg p-2 font-mono text-xs whitespace-pre-wrap">
            {request.command}
          </pre>
        )}
        {request.paths.length > 0 && (
          <ul className="max-h-32 overflow-auto rounded border border-border bg-bg p-2 font-mono text-xs">
            {request.paths.map((p) => (
              <li key={p}>{p}</li>
            ))}
          </ul>
        )}
        <details open={showInput} onToggle={(e) => setShowInput(e.currentTarget.open)}>
          <summary className="cursor-pointer text-xs text-muted">input</summary>
          <pre className="mt-1 max-h-40 overflow-auto rounded border border-border bg-bg p-2 font-mono text-xs">
            {JSON.stringify(request.input, null, 2)}
          </pre>
        </details>

        <div className="rounded border border-border p-2">
          <div className="mb-1 flex items-center gap-2 text-xs text-muted">
            <span>Rule for the lasting answers:</span>
            {suggested.length > 1 && (
              <select
                className="input py-0 text-xs"
                aria-label="Suggested rule"
                value={ruleIndex}
                onChange={(e) => {
                  setRuleIndex(Number(e.target.value));
                  setEdited(undefined);
                }}
              >
                {suggested.map((r, i) => (
                  <option key={i} value={i}>
                    {describeRule(r)}
                  </option>
                ))}
              </select>
            )}
            {edited !== undefined && (
              <button type="button" className="hover:text-fg" onClick={() => setEdited(undefined)}>
                reset
              </button>
            )}
            <span className="grow" />
            <span className="font-mono">{describeRule(rule)}</span>
          </div>
          <RuleFields rule={rule} onChange={setEdited} idPrefix={`perm-${request.requestId}`} />
        </div>

        {error !== undefined && (
          <p className="text-sm text-bad" role="alert">
            {error}
          </p>
        )}
        <div className="flex flex-wrap gap-2">
          {CHOICES.map((c) => {
            const disabled = busy || (c.answer === "allow_workspace" && !hasWorkspace);
            return (
              <button
                key={c.answer}
                type="button"
                className={c.primary ? "btn-primary" : "btn"}
                disabled={disabled}
                onClick={() => void answer(c)}
                title={
                  c.answer === "allow_workspace" && !hasWorkspace
                    ? "the session has no workspace"
                    : `key: ${c.key}`
                }
              >
                {c.label} <span className="font-mono text-[10px] opacity-60">{c.key}</span>
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );
}
