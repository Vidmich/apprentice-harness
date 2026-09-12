import { forgetRun } from "../lib/playground";
import { useStore } from "../store";

/** Left pane: the Playground tabs and the (M01) sessions list placeholder. */
export default function Sidebar() {
  const tabs = useStore((s) => s.tabs);
  const activeTab = useStore((s) => s.activeTab);
  const addTab = useStore((s) => s.addTab);
  const closeTab = useStore((s) => s.closeTab);
  const selectTab = useStore((s) => s.selectTab);

  return (
    <aside className="flex flex-col gap-4 border-r border-border bg-panel p-3">
      <section>
        <div className="mb-2 flex items-center">
          <h2 className="text-xs tracking-wider text-muted uppercase">Playground</h2>
          <span className="grow" />
          <button className="btn px-2 py-0 text-xs" onClick={addTab} title="New Playground tab">
            +
          </button>
        </div>
        <ul className="flex flex-col gap-1">
          {tabs.map((t) => (
            <li
              key={t.id}
              className={`flex cursor-pointer items-center rounded px-2 py-1 text-sm ${
                t.id === activeTab ? "bg-bg" : "hover:bg-bg/50"
              }`}
              onClick={() => selectTab(t.id)}
            >
              <span className="grow truncate">{t.title}</span>
              <span className="text-xs text-muted">{badge(t.run.phase)}</span>
              {tabs.length > 1 && (
                <button
                  className="ml-2 text-xs text-muted hover:text-fg"
                  title="Close"
                  onClick={(e) => {
                    e.stopPropagation();
                    forgetRun(t.id);
                    closeTab(t.id);
                  }}
                >
                  ×
                </button>
              )}
            </li>
          ))}
        </ul>
      </section>
      <section>
        <h2 className="mb-2 text-xs tracking-wider text-muted uppercase">Sessions</h2>
        <p className="text-xs text-muted">(coming in M01)</p>
      </section>
    </aside>
  );
}

function badge(phase: string): string {
  switch (phase) {
    case "starting":
    case "running":
      return "●";
    case "cancelling":
      return "◐";
    default:
      return "";
  }
}
