import { useAgents } from "../context/AgentContext";
import { useConversations } from "../conversation/ConversationContext";
import { ChatTab } from "./ChatTab";
import ConversationRail from "./ConversationRail";
import { useT } from "../i18n";

export function ConversationsView() {
  const { t } = useT();
  const { agents } = useAgents();
  const { open, active, closeConversation } = useConversations();

  if (open.length === 0) return null;

  return (
    <div className="conv-surface">
      <ConversationRail />
      <div className="conv-panels">
        {open.map((name) => {
          const entry = agents.find((a) => a.name === name);
          const display = entry?.display_name ?? name;
          const status = entry?.status ?? "idle";
          return (
            <div
              key={name}
              className="conv-panel"
              style={{ display: active === name ? "flex" : "none" }}
            >
              <div className="conv-panel__head">
                <span className={`conv-status conv-status--${status}`} />
                <span className="conv-panel__title">{display}</span>
                <button
                  className="conv-panel__close"
                  title={t("chat.close")}
                  aria-label={t("chat.close")}
                  onClick={() => closeConversation(name)}
                >
                  ×
                </button>
              </div>
              <ChatTab agentName={name} displayName={display} />
            </div>
          );
        })}
      </div>
    </div>
  );
}
