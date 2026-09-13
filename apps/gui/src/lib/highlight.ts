// Syntax highlighting through Shiki's fine-grained bundle: the engine
// and the two GitHub themes load on the first code block, each grammar
// on its first use. Tokens carry both themes' colours as CSS variables
// (`--shiki-light`, `--shiki-dark`); `index.css` picks one by scheme.

import type { HighlighterCore, LanguageRegistration, ThemedToken } from "shiki/core";

type Loader = () => Promise<{ default: LanguageRegistration[] }>;

/** Grammar loaders by name (aliases resolve below). */
const LANGS: Record<string, Loader> = {
  bash: () => import("shiki/langs/bash.mjs"),
  c: () => import("shiki/langs/c.mjs"),
  cpp: () => import("shiki/langs/cpp.mjs"),
  csharp: () => import("shiki/langs/csharp.mjs"),
  css: () => import("shiki/langs/css.mjs"),
  diff: () => import("shiki/langs/diff.mjs"),
  dockerfile: () => import("shiki/langs/dockerfile.mjs"),
  go: () => import("shiki/langs/go.mjs"),
  html: () => import("shiki/langs/html.mjs"),
  ini: () => import("shiki/langs/ini.mjs"),
  java: () => import("shiki/langs/java.mjs"),
  javascript: () => import("shiki/langs/javascript.mjs"),
  json: () => import("shiki/langs/json.mjs"),
  jsx: () => import("shiki/langs/jsx.mjs"),
  kotlin: () => import("shiki/langs/kotlin.mjs"),
  makefile: () => import("shiki/langs/makefile.mjs"),
  markdown: () => import("shiki/langs/markdown.mjs"),
  php: () => import("shiki/langs/php.mjs"),
  powershell: () => import("shiki/langs/powershell.mjs"),
  python: () => import("shiki/langs/python.mjs"),
  ruby: () => import("shiki/langs/ruby.mjs"),
  rust: () => import("shiki/langs/rust.mjs"),
  sql: () => import("shiki/langs/sql.mjs"),
  swift: () => import("shiki/langs/swift.mjs"),
  toml: () => import("shiki/langs/toml.mjs"),
  tsx: () => import("shiki/langs/tsx.mjs"),
  typescript: () => import("shiki/langs/typescript.mjs"),
  xml: () => import("shiki/langs/xml.mjs"),
  yaml: () => import("shiki/langs/yaml.mjs"),
};

const ALIASES: Record<string, string> = {
  sh: "bash",
  shell: "bash",
  shellscript: "bash",
  zsh: "bash",
  console: "bash",
  "c++": "cpp",
  cs: "csharp",
  docker: "dockerfile",
  golang: "go",
  htm: "html",
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  jsonc: "json",
  json5: "json",
  kt: "kotlin",
  make: "makefile",
  md: "markdown",
  ps: "powershell",
  ps1: "powershell",
  pwsh: "powershell",
  py: "python",
  rb: "ruby",
  rs: "rust",
  ts: "typescript",
  yml: "yaml",
  patch: "diff",
};

export const LIGHT_THEME = "github-light";
export const DARK_THEME = "github-dark";

/** The grammar name for a fence's language tag, or `undefined` when there is none for it. */
export function resolveLang(lang: string | undefined): string | undefined {
  if (lang === undefined || lang === "") return undefined;
  const key = lang.toLowerCase();
  const name = ALIASES[key] ?? key;
  return name in LANGS ? name : undefined;
}

let core: Promise<HighlighterCore> | undefined;
const loaded = new Set<string>();
const loading = new Map<string, Promise<void>>();

function highlighter(): Promise<HighlighterCore> {
  core ??= (async () => {
    const [{ createHighlighterCore }, { createJavaScriptRegexEngine }, light, dark] =
      await Promise.all([
        import("shiki/core"),
        import("shiki/engine/javascript"),
        import("shiki/themes/github-light.mjs"),
        import("shiki/themes/github-dark.mjs"),
      ]);
    return createHighlighterCore({
      themes: [light.default, dark.default],
      langs: [],
      engine: createJavaScriptRegexEngine({ forgiving: true }),
    });
  })();
  return core;
}

async function ensureLang(h: HighlighterCore, name: string): Promise<void> {
  if (loaded.has(name)) return;
  let p = loading.get(name);
  if (p === undefined) {
    p = (async () => {
      const loader = LANGS[name];
      if (loader === undefined) return;
      const grammar = await loader();
      await h.loadLanguage(...grammar.default);
      loaded.add(name);
    })();
    loading.set(name, p);
  }
  await p;
}

/**
 * Tokenises `code` in `lang` (a resolved grammar name). Lines of tokens
 * with `htmlStyle` carrying the two themes' colours.
 */
export async function tokenize(code: string, lang: string): Promise<ThemedToken[][]> {
  const h = await highlighter();
  await ensureLang(h, lang);
  return h.codeToTokens(code, {
    lang,
    themes: { light: LIGHT_THEME, dark: DARK_THEME },
    defaultColor: false,
  }).tokens;
}

/** Highlighting is skipped above this size (a pasted log, a whole file). */
export const MAX_HIGHLIGHT_CHARS = 100_000;
