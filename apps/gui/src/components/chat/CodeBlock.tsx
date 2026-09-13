import type { ThemedToken } from "shiki/core";
import { memo, useEffect, useState } from "react";
import { MAX_HIGHLIGHT_CHARS, resolveLang, tokenize } from "../../lib/highlight";
import CopyButton from "./CopyButton";

interface Props {
  code: string;
  lang?: string | undefined;
  /** Still growing: highlighting is debounced so each delta does not re-tokenise. */
  streaming?: boolean;
}

/** A fenced code block: language label, copy button, Shiki colours once loaded. */
function CodeBlock({ code, lang, streaming = false }: Props) {
  const grammar = resolveLang(lang);
  const [tokens, setTokens] = useState<{ code: string; lines: ThemedToken[][] }>();

  useEffect(() => {
    if (grammar === undefined || code.length > MAX_HIGHLIGHT_CHARS) return;
    let live = true;
    const run = () => {
      tokenize(code, grammar).then(
        (lines) => {
          if (live) setTokens({ code, lines });
        },
        (e: unknown) => console.warn("highlight failed:", e),
      );
    };
    const timer = setTimeout(run, streaming ? 150 : 0);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [code, grammar, streaming]);

  // Between deltas the last tokens stay and the new tail is plain: no
  // flash back to unstyled text.
  let lines: ThemedToken[][] | undefined;
  let tail = "";
  if (tokens !== undefined && code.startsWith(tokens.code)) {
    lines = tokens.lines;
    tail = code.slice(tokens.code.length);
  }
  return (
    <div className="my-2 rounded border border-border bg-panel">
      <div className="flex items-center gap-2 border-b border-border px-2 py-0.5 text-xs text-muted">
        <span>{lang ?? "text"}</span>
        <span className="grow" />
        <CopyButton text={code} />
      </div>
      <pre className="code overflow-x-auto p-2 font-mono text-[13px] leading-5">
        {lines !== undefined ? (
          <>
            {lines.map((line, i) => (
              <span key={i} className="line">
                {line.map((t, j) => (
                  <span key={j} style={t.htmlStyle as React.CSSProperties}>
                    {t.content}
                  </span>
                ))}
                {i < lines.length - 1 ? "\n" : ""}
              </span>
            ))}
            {tail}
          </>
        ) : (
          <code>{code}</code>
        )}
      </pre>
    </div>
  );
}

export default memo(CodeBlock);
