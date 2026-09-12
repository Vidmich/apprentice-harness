import Playground from "./components/Playground";
import Setup from "./components/Setup";
import Sidebar from "./components/Sidebar";
import StatusBar from "./components/StatusBar";
import { authConfigured, useStore } from "./store";

export default function App() {
  const daemon = useStore((s) => s.daemon);
  const auth = useStore((s) => s.auth);
  const tabs = useStore((s) => s.tabs);
  const activeTab = useStore((s) => s.activeTab);
  const configured = authConfigured(auth);
  const tab = tabs.find((t) => t.id === activeTab) ?? tabs[0];

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
  } else if (tab !== undefined) {
    main = <Playground key={tab.id} tab={tab} />;
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
