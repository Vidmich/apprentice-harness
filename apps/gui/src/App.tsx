import Chat from "./components/Chat";
import Setup from "./components/Setup";
import Sidebar from "./components/Sidebar";
import StatusBar from "./components/StatusBar";
import { authConfigured, useStore } from "./store";

export default function App() {
  const daemon = useStore((s) => s.daemon);
  const auth = useStore((s) => s.auth);
  const chats = useStore((s) => s.chats);
  const activeChat = useStore((s) => s.activeChat);
  const configured = authConfigured(auth);
  const chat = chats.find((c) => c.id === activeChat) ?? chats[0];

  let main: React.ReactNode;
  if (!daemon.connected) {
    main = (
      <div className="mx-auto mt-16 max-w-md text-center text-sm text-muted">
        <p className="mb-2 text-base text-fg">Connecting to the daemon…</p>
        {daemon.error !== undefined && <p>{daemon.error}</p>}
      </div>
    );
  } else if (configured === false) {
    main = <Setup />;
  } else if (chat !== undefined) {
    main = <Chat key={chat.id} chat={chat} />;
  }

  return (
    <div className="grid h-full grid-cols-[240px_1fr] grid-rows-[1fr_auto]">
      <Sidebar />
      <main className="min-h-0 overflow-hidden">{main}</main>
      <div className="col-span-2">
        <StatusBar />
      </div>
    </div>
  );
}
