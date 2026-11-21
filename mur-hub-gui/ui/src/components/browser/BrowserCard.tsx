//! The browser request card, as a modal. Every row, reason and port line is
//! the backend's (`agent_browser_card`); nothing here decides what is allowed.

import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import { newTaskId } from "../ChatTab";
import { type AnsweredView, type CardView, type PickArg, browserApi } from "./browserApi";
import { BrowserCardBody } from "./BrowserCardBody";

export function BrowserCard({
  agent,
  site,
  onClose,
}: {
  agent: string;
  site: string;
  onClose: (changed: boolean) => void;
}) {
  const { t } = useT();
  const [card, setCard] = useState<CardView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [loggingIn, setLoggingIn] = useState(false);
  const [result, setResult] = useState<AnsweredView | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const idRef = useRef<string | null>(null);

  useEffect(() => {
    let alive = true;
    browserApi
      .card(agent, site)
      .then((c) => {
        idRef.current = c.id;
        if (alive) setCard(c);
      })
      .catch((e) => alive && setError(String(e)));
    return () => {
      alive = false;
    };
  }, [agent, site]);

  function handBack(text: string) {
    setNote(t("browser.handBack.asking", { agent }));
    invoke("agent_chat_send", { name: agent, text, taskId: newTaskId(), contextTaskId: null, channelId: null })
      .then(() => setNote(t("browser.handBack.done", { agent })))
      .catch((e) => setNote(t("browser.handBack.failed", { agent, err: String(e) })));
  }

  function answer(pick: PickArg) {
    if (!card) return;
    setBusy(true);
    setError(null);
    setLoggingIn(pick.kind === "login");
    browserApi
      .answer(card.id, pick)
      .then((r) => {
        setResult(r);
        if (r.state === "done" && r.hand_back) handBack(r.hand_back);
      })
      .catch((e) => setError(String(e)))
      .finally(() => {
        setBusy(false);
        setLoggingIn(false);
      });
  }

  function close() {
    // Closing while the login browser is open cancels the login: nothing is saved.
    if (idRef.current) void browserApi.close(idRef.current);
    onClose(result?.state === "done");
  }

  return (
    <div className="modal__overlay">
      <div className="modal ssh-card browser-card" role="dialog" aria-modal="true">
        <div className="modal__header">
          <h2 className="modal__title">{t("browser.card.title", { agent, site })}</h2>
        </div>
        <div className="modal__body">
          {card?.reason && (
            <>
              <p className="field-muted">{t("browser.card.reasonIntro")}</p>
              <blockquote className="ssh-reason">{card.reason}</blockquote>
            </>
          )}
          {!card && !error && <p className="field-muted">{t("browser.card.loading")}</p>}
          {error && <p className="save-error">{error}</p>}
          {card && (
            <BrowserCardBody
              card={card}
              busy={busy}
              loggingIn={loggingIn}
              result={result}
              note={note}
              onAnswer={answer}
              onLoggedIn={() => void browserApi.loggedIn(card.id).catch((e) => setError(String(e)))}
            />
          )}
        </div>
        <div className="modal__footer">
          <button type="button" className="btn btn--secondary" onClick={close}>
            {t("browser.close")}
          </button>
        </div>
      </div>
    </div>
  );
}
