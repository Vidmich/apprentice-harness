import { useState } from "react";
import type { ConfigLayer, ConfigSource } from "../../lib/api";
import { RpcFailure } from "../../lib/rpc";
import { type Field, type Setting, fromText, saveSetting, toText } from "../../lib/settings";

const SOURCE_STYLE: Record<ConfigSource, string> = {
  default: "text-muted",
  user: "text-accent",
  workspace: "text-ok",
  env: "text-warn",
};

/**
 * One config key: its input, where the value comes from, and a reset
 * that takes the layer's value out. Writes on change (selects, boxes)
 * or on blur / Enter (text).
 */
export default function SettingField({
  field,
  setting,
  layer,
  workspace,
  onSaved,
}: {
  field: Field;
  setting: Setting | undefined;
  layer: ConfigLayer;
  workspace: string | undefined;
  onSaved: () => void;
}) {
  const stored = toText(field, setting?.value);
  const [text, setText] = useState(stored);
  const [shown, setShown] = useState(stored);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  // A fresh read replaces what is shown (the user layer's change lands).
  if (shown !== stored) {
    setShown(stored);
    setText(stored);
  }

  const locked = layer === "workspace" && field.workspace !== true;
  const source = setting?.source;
  const setHere = source === layer;

  const save = async (value: unknown) => {
    setBusy(true);
    setError(undefined);
    try {
      await saveSetting(field.key, value, layer, workspace);
      onSaved();
    } catch (e) {
      setError(e instanceof RpcFailure ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const commitText = () => {
    if (text === stored) return;
    try {
      void save(fromText(field, text));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const id = `set-${field.key.replace(/\./g, "-")}`;
  const disabled = busy || locked;
  let input: React.ReactNode;
  switch (field.kind) {
    case "select":
      input = (
        <select
          id={id}
          className="input"
          value={text}
          disabled={disabled}
          onChange={(e) => {
            setText(e.target.value);
            void save(e.target.value === "" ? null : e.target.value);
          }}
        >
          {source !== layer && <option value="">(inherited: {stored || "default"})</option>}
          {field.options?.map((o) => (
            <option key={o} value={o}>
              {o}
            </option>
          ))}
        </select>
      );
      break;
    case "bool":
      input = (
        <input
          id={id}
          type="checkbox"
          checked={text === "true"}
          disabled={disabled}
          onChange={(e) => {
            setText(String(e.target.checked));
            void save(e.target.checked);
          }}
        />
      );
      break;
    case "lines":
      input = (
        <textarea
          id={id}
          className="input min-h-16 font-mono"
          rows={3}
          value={text}
          placeholder={field.placeholder}
          disabled={disabled}
          onChange={(e) => setText(e.target.value)}
          onBlur={commitText}
        />
      );
      break;
    default:
      input = (
        <input
          id={id}
          className={`input ${field.kind === "number" ? "w-32" : ""}`}
          type={field.kind === "number" ? "number" : "text"}
          min={field.min}
          value={text}
          placeholder={field.placeholder}
          disabled={disabled}
          onChange={(e) => setText(e.target.value)}
          onBlur={commitText}
          onKeyDown={(e) => {
            if (e.key === "Enter") commitText();
          }}
        />
      );
  }

  return (
    <div className="flex flex-col gap-1 py-2">
      <div className="flex items-center gap-2">
        <label htmlFor={id} className="w-48 shrink-0 text-sm">
          {field.label}
        </label>
        <div className="flex grow items-center gap-2">
          {input}
          {source !== undefined && (
            <span
              className={`text-[11px] ${SOURCE_STYLE[source]}`}
              title="where the value in force comes from"
            >
              {source}
            </span>
          )}
          {setHere && !locked && (
            <button
              type="button"
              className="text-xs text-muted hover:text-fg"
              title={`Remove from the ${layer} config (the default or lower layer returns)`}
              disabled={busy}
              onClick={() => void save(null)}
            >
              reset
            </button>
          )}
        </div>
      </div>
      {(field.help !== undefined || locked) && (
        <p className="pl-50 text-xs text-muted">
          {locked ? "Not overridable per workspace." : field.help}
        </p>
      )}
      {error !== undefined && (
        <p className="pl-50 text-xs text-bad" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
