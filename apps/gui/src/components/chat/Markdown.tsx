import { type ReactElement, type ReactNode, isValidElement, memo } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import { openExternal } from "../../lib/ui";
import CodeBlock from "./CodeBlock";

const plugins = [remarkGfm];

function codeChild(children: ReactNode): { code: string; lang: string | undefined } | undefined {
  if (!isValidElement(children)) return undefined;
  const el = children as ReactElement<{ className?: string; children?: ReactNode }>;
  const lang = /language-([\w+-]+)/.exec(el.props.className ?? "")?.[1];
  const raw = el.props.children;
  const code = typeof raw === "string" ? raw : Array.isArray(raw) ? raw.join("") : "";
  return { code: code.replace(/\n$/, ""), lang };
}

function componentsFor(streaming: boolean): Components {
  return {
    pre: ({ children }) => {
      const block = codeChild(children);
      if (block === undefined) return <pre>{children}</pre>;
      return <CodeBlock code={block.code} lang={block.lang} streaming={streaming} />;
    },
    code: ({ children, className }) => (
      <code className={`rounded bg-panel px-1 py-0.5 font-mono text-[0.9em] ${className ?? ""}`}>
        {children}
      </code>
    ),
    a: ({ href, children }) => (
      <a
        href={href}
        className="text-accent underline"
        onClick={(e) => {
          e.preventDefault();
          if (href !== undefined) void openExternal(href);
        }}
      >
        {children}
      </a>
    ),
    table: ({ children }) => (
      <div className="my-2 overflow-x-auto">
        <table className="border-collapse text-sm">{children}</table>
      </div>
    ),
    th: ({ children }) => (
      <th className="border border-border bg-panel px-2 py-1 text-left">{children}</th>
    ),
    td: ({ children }) => <td className="border border-border px-2 py-1">{children}</td>,
  };
}

const STATIC = componentsFor(false);
const STREAMING = componentsFor(true);

/** GitHub-flavoured markdown; code through Shiki, links to the OS browser. */
function Markdown({ text, streaming = false }: { text: string; streaming?: boolean }) {
  return (
    <div className="prose-chat">
      <ReactMarkdown remarkPlugins={plugins} components={streaming ? STREAMING : STATIC}>
        {text}
      </ReactMarkdown>
    </div>
  );
}

export default memo(Markdown);
