import { useAgents } from "../context/AgentContext";
import { useConversations } from "../conversation/ConversationContext";
import { attentionLevel } from "../conversation/reducer";

export default function ConversationRail() {
  const { agents } = useAgents();
  const { open, active, attention, focusConversation, closeConversation } =
    useConversations();

  if (open.length === 0) return null;

  return (
    <div className="conv-rail">
      {open.map((name) => {
        const entry = agents.find((a) => a.name === name);
        const display = entry?.display_name ?? name;
        const status = entry?.status ?? "idle";
        const attn = attention[name] ?? { unread: false, hitl: false };
        const level = attentionLevel(attn);

        return (
          <button
            key={name}
            className={`conv-item${active === name ? " conv-item--active" : ""}`}
            onClick={() => focusConversation(name)}
            title={display}
          >
            <span className={`conv-status conv-status--${status}`} />
            <span className="conv-name">{display}</span>
            {level !== "none" && (
              <span className={`conv-badge conv-badge--${level}`} aria-label={level} />
            )}
            <button
              className="conv-close"
              aria-label="Close conversation"
              onClick={(e) => {
                e.stopPropagation();
                closeConversation(name);
              }}
            >
              ×
            </button>
          </button>
        );
      })}
    </div>
  );
}
