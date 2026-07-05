import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import type { InboxItem } from "./inbox";
import type {
  RawHitl,
  RawInstall,
  RawCompanionEvent,
  RawBlockedItem,
} from "./useInbox";

interface Props {
  /** Already filtered by the caller's dismissed-set (single source of truth). */
  items: InboxItem[];
  /** Re-pull the inbox after an action resolves an item. */
  onRefresh: () => void;
  /** Dismiss an item for this session; owned by the caller so the badge stays in sync. */
  onDismiss: (item: InboxItem) => void;
}

/**
 * Unified inbox — one card per pending item, dispatched by kind. Hidden
 * entirely when there is nothing to act on. `items` arrives pre-filtered by
 * the caller's dismissed-set so this list and the sidebar/Dock badge never
 * drift apart.
 */
export function NeedsYou({ items, onRefresh, onDismiss }: Props) {
  const { t } = useT();

  if (items.length === 0) return null;

  return (
    <section className="home-section home-needs-you">
      <h2 className="home-section__title">{t("home.needsYou")}</h2>
      <div className="home-cards">
        {items.map((it) => {
          switch (it.kind) {
            case "hitl":
              return (
                <HitlInboxCard
                  key={`hitl:${it.id}`}
                  item={it}
                  raw={it.payload as RawHitl}
                  onResolved={onRefresh}
                />
              );
            case "install":
              return (
                <InstallInboxCard
                  key={`install:${it.id}`}
                  item={it}
                  raw={it.payload as RawInstall}
                />
              );
            case "companion":
              return (
                <CompanionInboxCard
                  key={`companion:${it.id}`}
                  item={it}
                  raw={it.payload as RawCompanionEvent}
                />
              );
            case "upgrade_blocked":
              return (
                <UpgradeBlockedCard
                  key={`upgrade:${it.id}`}
                  item={it}
                  raw={it.payload as RawBlockedItem}
                  onKeep={() => onDismiss(it)}
                />
              );
            default:
              return null;
          }
        })}
      </div>
    </section>
  );
}

// ── HITL card ───────────────────────────────────────────────────────────────
// The Home inbox rolls up CHANNEL/workflow HITL gates (`hitl_pending_list`),
// so it responds via `channel_hitl_respond` (same as `mur channel approve`),
// NOT the per-agent `agent_hitl_respond` that the in-chat HitlCard uses. It
// reuses the `.hitl-card` styling for a consistent look.
function HitlInboxCard({
  item,
  raw,
  onResolved,
}: {
  item: InboxItem;
  raw: RawHitl;
  onResolved: () => void;
}) {
  const { t } = useT();
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  async function respond(allow: boolean) {
    if (busy) return;
    setBusy(true);
    setErr(null);
    try {
      await invoke("channel_hitl_respond", {
        channelId: raw.channel_id,
        hitlId: raw.hitl_id,
        allow,
        reason: null,
      });
      onResolved();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="home-card hitl-card">
      <div className="hitl-card__header">
        <span className="hitl-card__icon">⏸</span>
        <span className="hitl-card__title">{item.title}</span>
        {raw.risk && <span className="home-card__tag">{raw.risk}</span>}
      </div>
      <div className="hitl-card__prompt">{item.subtitle}</div>
      <div className="hitl-card__actions">
        <button
          className="hitl-card__btn hitl-card__btn--allow"
          onClick={() => respond(true)}
          disabled={busy}
        >
          ✅ {t("home.hitl.approve")}
        </button>
        <button
          className="hitl-card__btn hitl-card__btn--deny"
          onClick={() => respond(false)}
          disabled={busy}
        >
          ⛔ {t("home.hitl.deny")}
        </button>
      </div>
      {err && <p className="home-card__err">{err}</p>}
    </div>
  );
}

// ── Install card ────────────────────────────────────────────────────────────
// The consent DIALOG (InstallInboxModal) is always mounted at the app root and
// auto-opens whenever an install request is pending, so this summary card just
// surfaces it in the inbox. The button re-triggers the watcher refresh so the
// modal is guaranteed present.
function InstallInboxCard({ item, raw }: { item: InboxItem; raw: RawInstall }) {
  const { t } = useT();
  return (
    <div className="home-card home-card--install">
      <div className="home-card__head">
        <span className="home-card__icon">📦</span>
        <span className="home-card__title">{item.title}</span>
        {raw.is_official && (
          <span className="home-card__tag home-card__tag--official">MUR</span>
        )}
      </div>
      <p className="home-card__body">{item.subtitle}</p>
      <div className="home-card__actions">
        <button
          className="btn btn--sm btn--primary"
          onClick={() =>
            invoke("install_inbox_list").catch(() => {})
          }
        >
          {t("home.install.open")}
        </button>
      </div>
    </div>
  );
}

// ── Companion card ──────────────────────────────────────────────────────────
// Reuses the CompanionInbox message styling (`.inbox-msg`).
function CompanionInboxCard({
  item,
  raw,
}: {
  item: InboxItem;
  raw: RawCompanionEvent;
}) {
  return (
    <div className="home-card inbox-msg inbox-msg--unread">
      <div className="inbox-msg-header">
        <span className="inbox-situation">{item.title}</span>
        <span className="inbox-time">
          {new Date(raw.generated_at).toLocaleTimeString([], {
            hour: "2-digit",
            minute: "2-digit",
          })}
        </span>
      </div>
      <p className="inbox-body">{item.subtitle}</p>
    </div>
  );
}

// ── Blocked-upgrade card ────────────────────────────────────────────────────
// keep = dismiss for this session (local). overwrite = there is no single-skill
// apply command, so we surface the exact CLI to run rather than inventing
// backend. No local↔registry diff command exists, so no diff button.
function UpgradeBlockedCard({
  item,
  raw,
  onKeep,
}: {
  item: InboxItem;
  raw: RawBlockedItem;
  onKeep: () => void;
}) {
  const { t } = useT();
  const [confirming, setConfirming] = useState(false);

  return (
    <div className="home-card home-card--upgrade">
      <div className="home-card__head">
        <span className="home-card__icon">⚠️</span>
        <span className="home-card__title">{item.title}</span>
      </div>
      <p className="home-card__body">{item.subtitle}</p>
      {confirming ? (
        <div className="home-card__confirm">
          <p className="home-card__title-sm">{t("home.upgrade.overwriteTitle")}</p>
          <p className="home-card__body">{t("home.upgrade.overwriteHint")}</p>
          <div className="home-card__actions">
            <button
              className="btn btn--sm btn--secondary"
              onClick={() => setConfirming(false)}
            >
              {t("home.upgrade.dismiss")}
            </button>
          </div>
        </div>
      ) : (
        <div className="home-card__actions">
          <button className="btn btn--sm btn--secondary" onClick={onKeep}>
            {t("home.upgrade.keep")}
          </button>
          <button
            className="btn btn--sm btn--primary"
            onClick={() => setConfirming(true)}
            title={raw.dir}
          >
            {t("home.upgrade.overwrite")}
          </button>
        </div>
      )}
    </div>
  );
}
