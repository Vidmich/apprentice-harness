import { forwardRef, useImperativeHandle, useRef, useState } from "react";
import { type SendKey, useStore } from "../../store";

/** A paste at least this long becomes an attached block instead of inline text. */
export const PASTE_ATTACH_CHARS = 1500;
const PASTE_ATTACH_LINES = 12;

export interface ComposerHandle {
  focus(): void;
}

interface Props {
  /** `undefined` disables the box with the hint. */
  disabledHint?: string | undefined;
  running: boolean;
  cancelling: boolean;
  onSend: (prompt: string) => void;
  onCancel: () => void;
}

/** The prompt as sent: the typed text, then each pasted block. */
export function composePrompt(text: string, attachments: string[]): string {
  const parts = [text.trim()];
  for (const a of attachments) parts.push(`<pasted_text>\n${a}\n</pasted_text>`);
  return parts.filter((p) => p !== "").join("\n\n");
}

const SEND_LABEL: Record<SendKey, string> = { enter: "Enter", ctrl_enter: "Ctrl+Enter" };

/**
 * Multiline prompt box: Enter sends (Shift+Enter breaks the line) or
 * Ctrl+Enter by preference, large pastes become chips, Esc cancels a run.
 */
const Composer = forwardRef<ComposerHandle, Props>(function Composer(
  { disabledHint, running, cancelling, onSend, onCancel },
  ref,
) {
  const sendKey = useStore((s) => s.sendKey);
  const setSendKey = useStore((s) => s.setSendKey);
  const [text, setText] = useState("");
  const [attachments, setAttachments] = useState<string[]>([]);
  const area = useRef<HTMLTextAreaElement>(null);
  useImperativeHandle(ref, () => ({ focus: () => area.current?.focus() }), []);

  const disabled = disabledHint !== undefined;
  const canSend = !disabled && !running && composePrompt(text, attachments) !== "";

  const submit = () => {
    if (!canSend) return;
    onSend(composePrompt(text, attachments));
    setText("");
    setAttachments([]);
    if (area.current) area.current.style.height = "";
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Escape" && running) {
      e.preventDefault();
      onCancel();
      return;
    }
    if (e.key !== "Enter") return;
    const wantsSend =
      sendKey === "enter" ? !e.shiftKey && !e.ctrlKey && !e.metaKey : e.ctrlKey || e.metaKey;
    if (wantsSend) {
      e.preventDefault();
      submit();
    }
  };

  const onPaste = (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
    const pasted = e.clipboardData.getData("text/plain");
    const lines = pasted.split("\n").length;
    if (pasted.length >= PASTE_ATTACH_CHARS || lines >= PASTE_ATTACH_LINES) {
      e.preventDefault();
      setAttachments((a) => [...a, pasted]);
    }
  };

  const grow = (el: HTMLTextAreaElement) => {
    el.style.height = "";
    el.style.height = `${Math.min(el.scrollHeight, 240)}px`;
  };

  return (
    <div className="border-t border-border p-3">
      {attachments.length > 0 && (
        <ul className="mb-2 flex flex-wrap gap-1" aria-label="Attached text">
          {attachments.map((a, i) => (
            <li
              key={i}
              className="flex items-center gap-1 rounded-full border border-border bg-panel px-2 py-0.5 text-xs"
            >
              <span title={a.slice(0, 500)}>
                pasted text · {a.split("\n").length} lines · {a.length.toLocaleString("en-US")}{" "}
                chars
              </span>
              <button
                type="button"
                className="text-muted hover:text-fg"
                aria-label="Remove attachment"
                onClick={() => setAttachments((all) => all.filter((_, j) => j !== i))}
              >
                ×
              </button>
            </li>
          ))}
        </ul>
      )}
      <div className="flex items-end gap-2">
        <textarea
          ref={area}
          className="input min-h-[2.5rem] grow resize-none font-sans leading-5"
          rows={1}
          placeholder={disabledHint ?? `Ask the mentor… (${SEND_LABEL[sendKey]} to send)`}
          aria-label="Prompt"
          value={text}
          disabled={disabled}
          onChange={(e) => {
            setText(e.target.value);
            grow(e.target);
          }}
          onKeyDown={onKeyDown}
          onPaste={onPaste}
        />
        {running ? (
          <button
            type="button"
            className="btn"
            onClick={onCancel}
            disabled={cancelling}
            aria-label="Cancel the run"
          >
            {cancelling ? "Cancelling…" : "Cancel"}
          </button>
        ) : (
          <button type="button" className="btn-primary" onClick={submit} disabled={!canSend}>
            Send
          </button>
        )}
      </div>
      <div className="mt-1 flex items-center gap-3 text-[11px] text-muted">
        <button
          type="button"
          className="hover:text-fg"
          onClick={() => setSendKey(sendKey === "enter" ? "ctrl_enter" : "enter")}
          title="Switch what sends the prompt"
        >
          {sendKey === "enter" ? "Enter sends · Shift+Enter for a new line" : "Ctrl+Enter sends"}
        </button>
        <span>Ctrl+L focuses · Esc cancels</span>
      </div>
    </div>
  );
});

export default Composer;
