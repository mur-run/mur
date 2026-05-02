import { useCompanionBridge } from "./useCompanionBridge";
import { MessageRow } from "./MessageRow";

export function CompanionSidebar({ agent }: { agent: string }) {
  const { messages } = useCompanionBridge(agent);
  const unread = messages.filter((m) => m.response.kind === "unset").length;

  return (
    <aside
      aria-label="Companion inbox"
      className="flex h-full w-80 flex-col border-l border-neutral-700"
    >
      <header className="border-b border-neutral-700 p-3">
        <h2 className="text-sm font-semibold">Companion</h2>
        <div className="text-xs text-neutral-400">
          {unread} unread · {messages.length} total
        </div>
      </header>
      <div className="flex-1 overflow-y-auto">
        {messages.length === 0 ? (
          <p className="p-3 text-sm text-neutral-500">No messages yet.</p>
        ) : (
          messages
            .slice()
            .reverse()
            .map((m) => <MessageRow key={m.id} agent={agent} msg={m} />)
        )}
      </div>
    </aside>
  );
}
