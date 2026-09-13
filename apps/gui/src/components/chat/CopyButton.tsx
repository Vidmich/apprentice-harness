import { useEffect, useState } from "react";
import { copyText } from "../../lib/ui";

/** Copies `text` (or what `get` returns) and says so for a moment. */
export default function CopyButton({
  text,
  get,
  label = "copy",
  className = "",
}: {
  text?: string;
  get?: () => string;
  label?: string;
  className?: string;
}) {
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  useEffect(() => {
    if (state === "idle") return;
    const t = setTimeout(() => setState("idle"), 1500);
    return () => clearTimeout(t);
  }, [state]);
  const onClick = async (e: React.MouseEvent) => {
    e.stopPropagation();
    const value = get !== undefined ? get() : (text ?? "");
    setState((await copyText(value)) ? "copied" : "failed");
  };
  return (
    <button
      type="button"
      className={`rounded px-1.5 py-0 text-xs text-muted hover:bg-bg hover:text-fg ${className}`}
      onClick={(e) => void onClick(e)}
      aria-label={`Copy ${label}`}
      title="Copy to clipboard"
    >
      {state === "copied" ? "copied" : state === "failed" ? "copy failed" : label}
    </button>
  );
}
