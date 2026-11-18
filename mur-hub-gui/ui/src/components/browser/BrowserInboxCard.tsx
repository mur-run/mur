//! "Needs you" card for an agent's `browser_request`. Review opens the same
//! card flow as murmur; Dismiss is the card's own answer.

import { useState } from "react";
import { useT } from "../../i18n";
import type { InboxItem } from "../home/inbox";
import type { PendingView } from "./browserApi";
import { BrowserCard } from "./BrowserCard";

export function BrowserInboxCard({
  item,
  raw,
  onResolved,
  onPeekAgent,
}: {
  item: InboxItem;
  raw: PendingView;
  onResolved: () => void;
  onPeekAgent: (name: string) => void;
}) {
  const { t } = useT();
  const [open, setOpen] = useState(false);
  return (
    <div className="home-card hitl-card">
      <div className="hitl-card__header">
        <span className="hitl-card__icon">🌐</span>
        <span className="hitl-card__title">{item.title}</span>
        <button type="button" className="home-card__peek" onClick={() => onPeekAgent(raw.agent)}>
          {t("peek.viewConversation")}
        </button>
      </div>
      <blockquote className="ssh-reason">{raw.reason}</blockquote>
      <div className="hitl-card__actions">
        <button type="button" className="hitl-card__btn hitl-card__btn--allow" onClick={() => setOpen(true)}>
          {t("browser.open")}
        </button>
      </div>
      {open && (
        <BrowserCard
          agent={raw.agent}
          site={raw.site}
          onClose={(changed) => {
            setOpen(false);
            if (changed) onResolved();
          }}
        />
      )}
    </div>
  );
}
