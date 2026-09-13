import { forget } from "../lib/chat";
import { isBusy } from "../lib/transcript";
import { type Chat, useStore } from "../store";
import { useTranscripts } from "../stores/transcripts";

/** Left pane: the open chats and the (M01-12) sessions list placeholder. */
export default function Sidebar() {
  const chats = useStore((s) => s.chats);
  const activeChat = useStore((s) => s.activeChat);
  const addChat = useStore((s) => s.addChat);
  const closeChat = useStore((s) => s.closeChat);
  const selectChat = useStore((s) => s.selectChat);

  return (
    <aside className="flex flex-col gap-4 border-r border-border bg-panel p-3">
      <section>
        <div className="mb-2 flex items-center">
          <h2 className="text-xs tracking-wider text-muted uppercase">Chats</h2>
          <span className="grow" />
          <button className="btn px-2 py-0 text-xs" onClick={addChat} title="New chat">
            +
          </button>
        </div>
        <ul className="flex flex-col gap-1">
          {chats.map((c) => (
            <ChatRow
              key={c.id}
              chat={c}
              active={c.id === activeChat}
              closable={chats.length > 1}
              onSelect={() => selectChat(c.id)}
              onClose={() => {
                if (c.sessionId !== undefined) forget(c.sessionId);
                closeChat(c.id);
              }}
            />
          ))}
        </ul>
      </section>
      <section>
        <h2 className="mb-2 text-xs tracking-wider text-muted uppercase">Sessions</h2>
        <p className="text-xs text-muted">(coming in M01-12)</p>
      </section>
    </aside>
  );
}

function ChatRow({
  chat,
  active,
  closable,
  onSelect,
  onClose,
}: {
  chat: Chat;
  active: boolean;
  closable: boolean;
  onSelect: () => void;
  onClose: () => void;
}) {
  const transcript = useTranscripts((s) =>
    chat.sessionId === undefined ? undefined : s.transcripts[chat.sessionId],
  );
  const title = transcript?.info?.title ?? (chat.sessionId === undefined ? "New chat" : "Chat");
  const busy = transcript !== undefined && isBusy(transcript);
  return (
    <li
      className={`flex cursor-pointer items-center rounded px-2 py-1 text-sm ${active ? "bg-bg" : "hover:bg-bg/50"}`}
      onClick={onSelect}
      aria-current={active ? "page" : undefined}
    >
      <span className="grow truncate" title={title}>
        {title}
      </span>
      {busy && (
        <span className="text-xs text-accent" title="A run is in progress">
          ●
        </span>
      )}
      {closable && (
        <button
          className="ml-2 text-xs text-muted hover:text-fg"
          title="Close"
          aria-label="Close chat"
          onClick={(e) => {
            e.stopPropagation();
            onClose();
          }}
        >
          ×
        </button>
      )}
    </li>
  );
}
