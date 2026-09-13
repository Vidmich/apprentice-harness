// The settings screen's model: which config keys it shows, how each is
// edited, and the reads/writes behind it (`config.get` per key for the
// value and its source, `config.set` to the user or workspace layer,
// `null` to take a layer's value out again). The daemon validates and
// applies; a new session picks the values up (task M01-12).

import type { ConfigLayer, ConfigSource } from "./api";
import { call } from "./rpc";

export type FieldKind = "text" | "number" | "select" | "bool" | "lines";

export interface Field {
  /** Dotted config key. */
  key: string;
  label: string;
  kind: FieldKind;
  help?: string;
  /** For `select`. */
  options?: readonly string[];
  /** Shown when the value is the default (`text`, `number`). */
  placeholder?: string;
  /** May be set per workspace (`crates/core/src/config/schema.rs`'s whitelist). */
  workspace?: boolean;
  min?: number;
}

export interface Section {
  id: string;
  title: string;
  fields: Field[];
}

export const EFFORTS = ["low", "medium", "high", "xhigh", "max"] as const;

export const SECTIONS: Section[] = [
  {
    id: "mentor",
    title: "Mentor",
    fields: [
      {
        key: "mentor.model",
        label: "Model",
        kind: "text",
        placeholder: "claude-opus-5",
        help: "The Anthropic model id the mentor calls.",
        workspace: true,
      },
      {
        key: "mentor.effort",
        label: "Effort",
        kind: "select",
        options: EFFORTS,
        help: "How hard the mentor thinks; `high` is the default.",
        workspace: true,
      },
      {
        key: "mentor.thinking_display",
        label: "Thinking",
        kind: "select",
        options: ["summarized", "omitted"],
        help: "Show the mentor's thinking (summarised) or leave it out of the transcript.",
      },
      {
        key: "mentor.max_tokens",
        label: "Max output tokens",
        kind: "number",
        placeholder: "64000",
        min: 1,
        help: "Longest answer per mentor call.",
        workspace: true,
      },
    ],
  },
  {
    id: "permissions",
    title: "Permissions",
    fields: [
      {
        key: "permissions.default_mode",
        label: "Default mode",
        kind: "select",
        options: ["default", "plan", "auto"],
        help:
          "`default`: rules decide, you are asked for the rest. `plan`: writes and commands denied. " +
          "`auto`: writes inside the workspace allowed without asking.",
        workspace: true,
      },
      {
        key: "permissions.headless",
        label: "Nobody attached",
        kind: "select",
        options: ["deny", "allow_readonly"],
        help: "What a call that would be asked gets when no client follows the run.",
        workspace: true,
      },
      {
        key: "permissions.ask_timeout_s",
        label: "Ask timeout (s)",
        kind: "number",
        placeholder: "600",
        min: 1,
        help: "A request nobody answers in this time is denied.",
        workspace: true,
      },
    ],
  },
  {
    id: "shell",
    title: "Shell",
    fields: [
      {
        key: "tools.shell.program",
        label: "Program",
        kind: "text",
        placeholder: "(pwsh / powershell on Windows, $SHELL elsewhere)",
        help: "The program that runs a `shell` command.",
        workspace: true,
      },
      {
        key: "tools.shell.args",
        label: "Arguments",
        kind: "lines",
        placeholder: "(the program's own defaults)",
        help: "One per line, before the command.",
        workspace: true,
      },
      {
        key: "tools.shell.max_timeout_s",
        label: "Longest timeout (s)",
        kind: "number",
        placeholder: "3600",
        min: 1,
        help: "The most a call may ask for.",
        workspace: true,
      },
    ],
  },
  {
    id: "sessions",
    title: "Sessions",
    fields: [
      {
        key: "sessions.auto_title",
        label: "Name sessions automatically",
        kind: "bool",
        help: "After the first answer, the cheap model names the session (unless you did).",
      },
    ],
  },
  { id: "data", title: "Data", fields: [] },
  { id: "about", title: "About", fields: [] },
];

export interface Setting {
  value: unknown;
  source: ConfigSource | undefined;
}

/** Reads every field's value and source for `workspace` (or the user config). */
export async function loadSettings(
  fields: Field[],
  workspace?: string,
): Promise<Record<string, Setting>> {
  const out: Record<string, Setting> = {};
  await Promise.all(
    fields.map(async (f) => {
      const params = workspace === undefined ? { key: f.key } : { key: f.key, workspace };
      try {
        const got = await call("config.get", params);
        out[f.key] = { value: got.value, source: got.source };
      } catch {
        out[f.key] = { value: undefined, source: undefined };
      }
    }),
  );
  return out;
}

/** Writes one key to a layer; `null` removes it from that layer. */
export async function saveSetting(
  key: string,
  value: unknown,
  layer: ConfigLayer,
  workspace?: string,
): Promise<void> {
  const params =
    layer === "workspace" && workspace !== undefined
      ? { key, value, layer, workspace }
      : { key, value, layer };
  await call("config.set", params);
}

/** The field's value as text for its input. */
export function toText(field: Field, value: unknown): string {
  if (value === undefined || value === null) return "";
  if (field.kind === "lines") return Array.isArray(value) ? value.map(String).join("\n") : "";
  return String(value);
}

/**
 * The input's text as the value `config.set` takes, or `null` for an
 * empty text (the layer's value goes, the default returns).
 */
export function fromText(field: Field, text: string): unknown {
  const t = text.trim();
  if (t === "") return null;
  switch (field.kind) {
    case "number": {
      const n = Number(t);
      if (!Number.isFinite(n)) throw new Error(`${field.label}: not a number`);
      if (field.min !== undefined && n < field.min) {
        throw new Error(`${field.label}: at least ${field.min}`);
      }
      return Number.isInteger(n) ? n : Math.round(n);
    }
    case "lines":
      return text
        .split("\n")
        .map((l) => l.trim())
        .filter((l) => l !== "");
    case "bool":
      return t === "true";
    default:
      return t;
  }
}
