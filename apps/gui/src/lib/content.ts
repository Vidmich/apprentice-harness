// The content blocks of a stored message (`SessionMessage.content`), as
// the Messages API shapes them. The daemon stores them verbatim; this
// only narrows the JSON for rendering and skips what it does not know.

export interface TextBlock {
  type: "text";
  text: string;
}

export interface ThinkingBlock {
  type: "thinking";
  thinking: string;
  signature?: string;
}

export interface RedactedThinkingBlock {
  type: "redacted_thinking";
  data?: string;
}

export interface ToolUseBlock {
  type: "tool_use";
  id: string;
  name: string;
  input: unknown;
}

export interface ToolResultBlock {
  type: "tool_result";
  tool_use_id: string;
  /** A string, or blocks (`text` ones carry the text). */
  content?: string | unknown[];
  is_error?: boolean;
}

/** A block type this build does not render (an image, a document, something newer). */
export interface OtherBlock {
  type: "other";
  /** The block's own `type`. */
  original: string;
}

export type ContentBlock =
  TextBlock | ThinkingBlock | RedactedThinkingBlock | ToolUseBlock | ToolResultBlock | OtherBlock;

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null;
}

/** The blocks of a message; anything malformed is dropped. */
export function blocksOf(content: unknown): ContentBlock[] {
  if (!Array.isArray(content)) return [];
  const out: ContentBlock[] = [];
  for (const b of content) {
    if (!isRecord(b) || typeof b.type !== "string") continue;
    switch (b.type) {
      case "text":
        if (typeof b.text === "string") out.push({ type: "text", text: b.text });
        break;
      case "thinking":
        if (typeof b.thinking === "string") {
          out.push({ type: "thinking", thinking: b.thinking });
        }
        break;
      case "redacted_thinking":
        out.push({ type: "redacted_thinking" });
        break;
      case "tool_use":
        if (typeof b.id === "string" && typeof b.name === "string") {
          out.push({ type: "tool_use", id: b.id, name: b.name, input: b.input });
        }
        break;
      case "tool_result":
        if (typeof b.tool_use_id === "string") {
          const block: ToolResultBlock = { type: "tool_result", tool_use_id: b.tool_use_id };
          if (typeof b.content === "string" || Array.isArray(b.content)) {
            block.content = b.content;
          }
          if (b.is_error === true) block.is_error = true;
          out.push(block);
        }
        break;
      default:
        out.push({ type: "other", original: b.type });
    }
  }
  return out;
}

/** The text of a tool result as the mentor reads it. */
export function resultText(block: ToolResultBlock): string {
  if (typeof block.content === "string") return block.content;
  if (!Array.isArray(block.content)) return "";
  return block.content
    .filter((b): b is TextBlock => isRecord(b) && b.type === "text" && typeof b.text === "string")
    .map((b) => b.text)
    .join("\n");
}
