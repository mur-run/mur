import { useState } from "react";
import type { AgentEntry } from "../types";
import { ChatTab } from "./ChatTab";
import { PetFace } from "./PetFace";
import { avatarPreset, familyOf } from "../utils";
import { useT } from "../i18n";

interface Props {
  agents: AgentEntry[];
}

/**
 * The Chats surface: a 1:1 conversation home. Left = the agents you can talk to
 * (pet avatar + name); right = the live chat inline (reuses ChatTab — read AND
 * continue in place). This is where 1:1 chats live now that Activity is scoped
 * to multi-agent runs.
 */
export function ChatsView({ agents }: Props) {
  const { t } = useT();
  const [selected, setSelected] = useState<string | null>(agents[0]?.name ?? null);

  if (agents.length === 0) {
    return <div className="chats-view__empty">{t("chats.empty")}</div>;
  }

  const entry = agents.find((a) => a.name === selected) ?? agents[0];

  return (
    <div className="chats-view">
      <nav className="chats-view__list">
        {agents.map((a) => {
          const preset = avatarPreset(a);
          return (
            <button
              key={a.name}
              className={`chats-item${entry?.name === a.name ? " is-active" : ""}`}
              onClick={() => setSelected(a.name)}
            >
              <span className="chats-item__avatar">
                <PetFace
                  presetId={preset}
                  family={familyOf(preset)}
                  expression="idle"
                  size={30}
                  animate={false}
                />
              </span>
              <span className="chats-item__name">{a.display_name}</span>
            </button>
          );
        })}
      </nav>
      <div className="chats-view__main">
        {entry && (
          <ChatTab
            key={entry.name}
            agentName={entry.name}
            displayName={entry.display_name}
          />
        )}
      </div>
    </div>
  );
}
