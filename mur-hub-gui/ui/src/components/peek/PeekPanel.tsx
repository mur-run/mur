import { useEffect, useRef, useState } from "react";
import type { AgentEntry, AgentRuntimeStatus } from "../../types";
import type { ChannelSummary } from "../../work/types";
import { useT } from "../../i18n";
import { useConversations } from "../../conversation/ConversationContext";
import { buildChatList } from "../chats/chatList";
import { ChatPane } from "../chats/ChatPane";
import { FleetHost } from "../detail/fleet/FleetHost";
import { isEditingTarget, isOpenInWindowShortcut } from "../detail/window/openInWindow";
import type { PeekTarget } from "./peekModel";

export interface PeekPanelProps {
  target: PeekTarget;
  agents: AgentEntry[];
  runtimeMap: Map<string, AgentRuntimeStatus>;
  channels: ChannelSummary[];
  onClose: () => void;
  /** Leave Home for the page that owns the target, with it selected. */
  onGo: (t: PeekTarget) => void;
  /** The chat window or the fleet detail window; the caller closes the peek. */
  onOpenInWindow: (t: PeekTarget) => void;
  /** ChatPane's "Open agent": the Agents page with that agent selected. */
  onOpenAgent: (name: string) => void;
}

/** The right-side slide-over Home peeks into (spec 3(b) §5). Esc and the
 *  scrim close it; ⌘↩ opens the target in its window. */
export function PeekPanel({ target, agents, runtimeMap, channels, onClose, onGo, onOpenInWindow, onOpenAgent }: PeekPanelProps) {
  const { t } = useT();
  const closeRef = useRef<HTMLButtonElement>(null);
  const [fleetTitle, setFleetTitle] = useState<string | null>(null);

  // Focus the close button on open; give focus back on close.
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    closeRef.current?.focus();
    return () => previous?.focus();
  }, []);

  // Esc closes only the peek (capture phase, stopPropagation: the page's
  // global Esc must not also clear a selection); ⌘↩ opens in a window.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.stopPropagation();
        e.preventDefault();
        onClose();
      } else if (isOpenInWindowShortcut(e) && !isEditingTarget(document.activeElement)) {
        e.stopPropagation();
        e.preventDefault();
        onOpenInWindow(target);
      }
    }
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [target, onClose, onOpenInWindow]);

  const entry = target.kind === "chat" ? agents.find((a) => a.name === target.agent) : undefined;
  const title = target.kind === "chat" ? (entry?.display_name ?? target.agent) : (fleetTitle ?? target.name);

  return (
    <>
      <div className="peek__scrim" onClick={onClose} />
      <aside className="peek" role="dialog" aria-modal="true" aria-label={title}>
        <header className="peek__bar">
          <span className="peek__title">{title}</span>
          <div className="peek__actions">
            <button type="button" className="btn btn--secondary" onClick={() => onGo(target)}>
              {t("peek.go")}
            </button>
            <button type="button" className="btn btn--secondary" onClick={() => onOpenInWindow(target)}>
              {t("action.openInWindow")}
            </button>
            <button ref={closeRef} type="button" className="peek__close" onClick={onClose} aria-label={t("detail.close")}>
              ×
            </button>
          </div>
        </header>
        <div className="peek__body">
          {target.kind === "chat" ? (
            <ChatBody agent={target.agent} entry={entry} runtimeMap={runtimeMap} channels={channels} onOpenAgent={onOpenAgent} />
          ) : (
            <FleetHost
              name={target.name}
              initialTab="jobs"
              missing={<p className="peek__missing">{t("detailWindow.missingFleet")}</p>}
              onDeleted={onClose}
              onTitle={setFleetTitle}
            />
          )}
        </div>
      </aside>
    </>
  );
}

function ChatBody({ agent, entry, runtimeMap, channels, onOpenAgent }: {
  agent: string;
  entry: AgentEntry | undefined;
  runtimeMap: Map<string, AgentRuntimeStatus>;
  channels: ChannelSummary[];
  onOpenAgent: (name: string) => void;
}) {
  const { t } = useT();
  const { attention, openConversation, focusConversation, blurConversation } = useConversations();

  // Reading the conversation: the Chats page's attention rule (spec 3(a) §4).
  useEffect(() => {
    openConversation(agent);
    focusConversation(agent);
    return () => blurConversation();
  }, [agent, openConversation, focusConversation, blurConversation]);

  if (!entry) return <p className="peek__missing">{t("detailWindow.missingAgent")}</p>;
  const item = buildChatList([entry], attention, channels)[0];
  return <ChatPane item={item} runtime={runtimeMap.get(agent)} onOpenAgent={onOpenAgent} />;
}
