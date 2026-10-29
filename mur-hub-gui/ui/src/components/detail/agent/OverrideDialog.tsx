import { useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { useModalFocus } from "../../useModalFocus";
import { useT } from "../../../i18n";
import type { OverrideReply } from "../../../types";
import { NO_CHOICE, allChosen, anyChosen, canContinue, toggleAll, type OverrideChoice } from "./overrideLogic";

type Result = { ok: boolean; text: string };

/** Pick override flags, read the CLI's banner, type the name, then hand the
 *  request to the Hub — which still asks a native confirmation before
 *  anything changes. The banner text always comes from Rust. */
export function OverrideDialog({ agentName, onClose }: { agentName: string; onClose: () => void }) {
  const { t } = useT();
  const [choice, setChoice] = useState<OverrideChoice>(NO_CHOICE);
  const [typed, setTyped] = useState("");
  const [banner, setBanner] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<Result | null>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const titleId = useId();
  useModalFocus(dialogRef);

  // Busy disables the button that was just pressed, which drops focus to
  // <body>; park it on the dialog so keyboard focus never leaves.
  useEffect(() => {
    const d = dialogRef.current;
    if (d && !d.contains(document.activeElement)) d.focus();
  }, [busy, result]);

  useEffect(() => {
    if (!anyChosen(choice)) {
      setBanner(null);
      return;
    }
    let live = true;
    invoke<string>("override_banner", { name: agentName, ...choice })
      .then((b) => { if (live) setBanner(b); })
      .catch((e) => { if (live) setBanner(String(e)); });
    return () => { live = false; };
  }, [agentName, choice]);

  // Escape closes, except mid-restart: the outcome must be seen.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape" && !busy) onClose();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busy, onClose]);

  async function submit() {
    setBusy(true);
    try {
      const reply = await invoke<OverrideReply>("restart_agent_with_overrides", { name: agentName, ...choice });
      setResult({ ok: reply.outcome.kind === "applied", text: reply.message });
    } catch (e) {
      setResult({ ok: false, text: String(e) });
    } finally {
      setBusy(false);
    }
  }

  const flag = (key: keyof OverrideChoice, label: string) => (
    <label className="override-dialog__flag">
      <input
        type="checkbox"
        checked={choice[key]}
        disabled={busy || result !== null}
        onChange={(e) => setChoice({ ...choice, [key]: e.target.checked })}
      />
      {label}
    </label>
  );

  return createPortal(
    <div className="modal__overlay" onClick={() => { if (!busy) onClose(); }}>
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-busy={busy}
        tabIndex={-1}
        className="modal modal--wide override-dialog"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal__header">
          <h2 id={titleId} className="modal__title">{t("override.dialog.title", { name: agentName })}</h2>
          {!busy && (
            <button className="modal__close" onClick={onClose} aria-label={t("detail.close")}>
              ×
            </button>
          )}
        </div>
        <div className="modal__body">
          {flag("noHitl", t("override.dialog.noHitl"))}
          {flag("netUnrestricted", t("override.dialog.netUnrestricted"))}
          {flag("fullPerms", t("override.dialog.fullPerms"))}
          <label className="override-dialog__flag override-dialog__all">
            <input
              type="checkbox"
              checked={allChosen(choice)}
              disabled={busy || result !== null}
              onChange={() => setChoice(toggleAll(choice))}
            />
            {t("override.dialog.all")}
          </label>
          {banner !== null && <pre className="override-dialog__banner">{banner}</pre>}
          <label className="field-muted" htmlFor="override-confirm-name">{t("override.dialog.typeName")}</label>
          <input
            id="override-confirm-name"
            className="input"
            autoComplete="off"
            spellCheck={false}
            value={typed}
            disabled={busy || result !== null}
            onChange={(e) => setTyped(e.target.value)}
          />
          {busy && <p className="override-dialog__busy" role="status">{t("override.dialog.busy", { name: agentName })}</p>}
          {result && (
            <p role="status" className={result.ok ? "override-dialog__result--ok" : "override-dialog__result--err"}>{result.text}</p>
          )}
        </div>
        <div className="modal__footer">
          {result === null ? (
            <button
              type="button"
              className="btn btn--danger"
              disabled={busy || !canContinue(choice, typed, agentName)}
              onClick={() => { void submit(); }}
            >
              {t("override.dialog.continue")}
            </button>
          ) : (
            <button type="button" className="btn btn--secondary" onClick={onClose}>
              {t("common.close")}
            </button>
          )}
        </div>
      </div>
    </div>,
    document.body,
  );
}
