import { useAgents } from "../context/AgentContext";
import { useConversations } from "../conversation/ConversationContext";
import { ChatTab } from "./ChatTab";
import ConversationRail from "./ConversationRail";

export function ConversationsView() {
  const { agents } = useAgents();
  const { open, active } = useConversations();

  if (open.length === 0) return null;

  return (
    <div className="conv-surface">
      <ConversationRail />
      <div className="conv-panels">
        {open.map((name) => {
          const display = agents.find((a) => a.name === name)?.display_name ?? name;
          return (
            <div
              key={name}
              className="conv-panel"
              style={{ display: active === name ? "flex" : "none" }}
            >
              <ChatTab agentName={name} displayName={display} />
            </div>
          );
        })}
      </div>
    </div>
  );
}
