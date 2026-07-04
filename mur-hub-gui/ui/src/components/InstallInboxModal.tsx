//! Consent modal for relay `install_request` events (Plan
//! `2026-07-04-relay-one-click-install.md` Task 3): the Dashboard's
//! "Install to Hub" button lands a pending request in
//! `<mur_home>/hub/install-requests.jsonl`; this component live-tails
//! it (via the `install-inbox-updated` event the Rust watcher emits)
//! and lets the user Approve/Deny each one. Fail-closed: nothing
//! installs without an explicit Approve click.

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useT } from "../i18n";

interface InstallRequestView {
  install_type: string;
  id: string;
  publisher: string;
  request_id: string;
  requested_at: number;
  is_official: boolean;
}

export function InstallInboxModal() {
  const { t } = useT();
  const [pending, setPending] = useState<InstallRequestView[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  function refresh() {
    invoke<InstallRequestView[]>("install_inbox_list")
      .then(setPending)
      .catch((e) => setError(String(e)));
  }

  useEffect(() => {
    refresh();
    const unlisten = listen("install-inbox-updated", refresh);
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  async function respond(requestId: string, approve: boolean) {
    setError(null);
    setBusy(requestId);
    try {
      await invoke("install_inbox_consent", { requestId, approve });
      refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  }

  if (pending.length === 0) return null;

  const req = pending[0];

  return (
    <div className="modal__overlay">
      <div className="modal">
        <div className="modal__header">
          <h2 className="modal__title">{t("installInbox.title")}</h2>
        </div>
        <div className="modal__body">
          {error && <p className="save-error">{error}</p>}
          <div className="item-card">
            <div className="item-card-name">
              {req.id}
              {req.is_official && (
                <span className="badge-sm badge-sm--official">{t("installInbox.official")}</span>
              )}
            </div>
            <p className="field-muted">
              {t("installInbox.type")}: {req.install_type} · {t("installInbox.publisher")}: {req.publisher}
            </p>
            <p className="field-muted">{t("installInbox.scanPlaceholder")}</p>
          </div>
          {pending.length > 1 && (
            <p className="field-muted">{t("installInbox.queued", { count: pending.length - 1 })}</p>
          )}
        </div>
        <div className="modal__footer" style={{ padding: 12, display: "flex", gap: 8, justifyContent: "flex-end" }}>
          <button
            className="btn btn--sm btn--secondary"
            disabled={busy !== null}
            onClick={() => respond(req.request_id, false)}
          >
            {busy === req.request_id ? t("detail.saving") : t("installInbox.deny")}
          </button>
          <button
            className="btn btn--sm btn--primary"
            disabled={busy !== null}
            onClick={() => respond(req.request_id, true)}
          >
            {busy === req.request_id ? t("detail.saving") : t("installInbox.approve")}
          </button>
        </div>
      </div>
    </div>
  );
}
