import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AgentEntry } from "../../types";
import { ChatTab } from "../ChatTab";
import { PetFace } from "../PetFace";
import { avatarPreset, familyOf } from "../../utils";
import { useT } from "../../i18n";
import { useConversations } from "../../conversation/ConversationContext";
import { buildChatList } from "./chatList";

interface Props {
  agents: AgentEntry[];
  query?: string;
  /** Reports the active chat's agent up so the Shell inspector can show it. */
  onActiveChange?: (agentName: string | null, displayName?: string) => void;
  /** Agent to open when the page is entered from elsewhere (Agents → Chat). */
  initialAgent?: string | null;
}

/**
 * Unified Chats surface (merges the former ConversationsView + ChatsView).
 * Left = every agent you can talk to (pet avatar + name, searchable, with
 * unread / HITL attention badges); right = the live chat inline via ChatTab
 * (read AND continue in place). Each row also has a pop-out button that opens
 * the agent in its own `#/chat/<name>` window via the `open_chat_window`
 * Tauri command. The thread pane reuses `components/chat/*` untouched, so
 * streaming, image paste, suggested replies, autocomplete, in-thread HITL
 * cards, and per-connection stream isolation all behave exactly as before.
 */
export function ChatsPage({ agents, query, onActiveChange, initialAgent }: Props) {
  const { t } = useT();
  const { attention } = useConversations();
  const [selected, setSelected] = useState<string | null>(null);
  const [localQuery, setLocalQuery] = useState("");

  useEffect(() => {
    if (initialAgent) setSelected(initialAgent);
  }, [initialAgent]);

  const items = agents.length === 0 ? [] : buildChatList(agents, attention, [], localQuery || query);
  const active = items.find((i) => i.name === selected) ?? items[0];

  // Report the active chat up so DashboardApp can show the ChatInspector.
  useEffect(() => {
    onActiveChange?.(active?.name ?? null, active?.displayName);
    return () => onActiveChange?.(null);
  }, [active?.name, active?.displayName, onActiveChange]);

  if (agents.length === 0) {
    return <div className="chats-view__empty">{t("chats.empty")}</div>;
  }

  function popOut(name: string) {
    invoke("open_chat_window", { agentName: name }).catch(console.error);
  }

  return (
    <div className="chats-view">
      <nav className="chats-view__list">
        <input
          className="source-list__filter"
          type="search"
          value={localQuery}
          placeholder={t("chats.filter")}
          onChange={(e) => setLocalQuery(e.target.value)}
        />
        {items.map((item) => {
          const preset = avatarPreset(item.agent);
          const level = item.hitl ? "hitl" : item.unread ? "unread" : null;
          // Row is a div (not a button) because it holds an inner pop-out
          // control — nested <button>s are invalid HTML and break clicks.
          return (
            <div
              key={item.name}
              role="button"
              tabIndex={0}
              className={`chats-item${active?.name === item.name ? " is-active" : ""}`}
              onClick={() => setSelected(item.name)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  setSelected(item.name);
                }
              }}
              title={item.displayName}
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
              <span className="chats-item__name">{item.displayName}</span>
              {level && (
                <span
                  className={`conv-badge conv-badge--${level}`}
                  aria-label={level}
                />
              )}
              <button
                className="chats-item__popout"
                title={t("chat.popout")}
                aria-label={t("chat.popout")}
                onClick={(e) => {
                  e.stopPropagation();
                  popOut(item.name);
                }}
              >
                ↗
              </button>
            </div>
          );
        })}
      </nav>
      <div className="chats-view__main">
        {active && (
          <ChatTab
            key={active.name}
            agentName={active.name}
            displayName={active.displayName}
          />
        )}
      </div>
    </div>
  );
}
