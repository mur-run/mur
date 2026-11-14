//! "Needs you" card for an agent's `ssh_request` (SSH setup spec, Q11).
//! Set up opens the same card as the Permissions tab; Dismiss removes the
//! request file through `mur_ssh_request::remove`.

import { useState } from "react";
import { useT } from "../../i18n";
import type { InboxItem } from "../home/inbox";
import { type PendingView, sshApi } from "./sshApi";
import { SshCard } from "./SshCard";

export function SshInboxCard({
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
  const [err, setErr] = useState<string | null>(null);
  return (
    <div className="home-card hitl-card">
      <div className="hitl-card__header">
        <span className="hitl-card__icon">🔑</span>
        <span className="hitl-card__title">{item.title}</span>
        <button type="button" className="home-card__peek" onClick={() => onPeekAgent(raw.agent)}>
          {t("peek.viewConversation")}
        </button>
      </div>
      <blockquote className="ssh-reason">{raw.reason}</blockquote>
      <div className="hitl-card__actions">
        <button type="button" className="hitl-card__btn hitl-card__btn--allow" onClick={() => setOpen(true)}>
          {t("ssh.setUp")}
        </button>
        <button
          type="button"
          className="hitl-card__btn hitl-card__btn--deny"
          onClick={() => void sshApi.dismiss(raw.agent, raw.host, raw.port).then(onResolved).catch((e) => setErr(String(e)))}
        >
          {t("ssh.dismiss")}
        </button>
      </div>
      {err && <p className="home-card__err">{err}</p>}
      {open && (
        <SshCard
          target={{ agent: raw.agent, host: raw.host, port: raw.port, user: raw.user }}
          onClose={(changed) => {
            setOpen(false);
            if (changed) onResolved();
          }}
        />
      )}
    </div>
  );
}
