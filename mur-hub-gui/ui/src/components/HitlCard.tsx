import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { HitlRequest } from "../types";
import { useT } from "../i18n";

interface Props {
  request: HitlRequest;
}

export function HitlCard({ request }: Props) {
  const { t } = useT();
  const timeoutSecs = Math.floor(request.timeout_ms / 1000);
  const [remaining, setRemaining] = useState(timeoutSecs);
  const [responded, setResponded] = useState<"allowed" | "denied" | "timeout" | null>(null);
  const [busy, setBusy] = useState(false);
  const [showReasonInput, setShowReasonInput] = useState(false);
  const [reason, setReason] = useState("");
  const intervalRef = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    intervalRef.current = setInterval(() => {
      setRemaining((r) => {
        if (r <= 1) {
          clearInterval(intervalRef.current!);
          setResponded("timeout");
          return 0;
        }
        return r - 1;
      });
    }, 1000);
    return () => clearInterval(intervalRef.current!);
  }, []);

  async function respond(allow: boolean, denyReason?: string) {
    if (responded || busy) return;
    setBusy(true);
    clearInterval(intervalRef.current!);
    try {
      await invoke("agent_hitl_respond", {
        name: request.agent,
        hitlId: request.hitl_id,
        allow,
        reason: denyReason ?? null,
      });
      setResponded(allow ? "allowed" : "denied");
    } catch {
      setResponded(allow ? "allowed" : "denied");
    } finally {
      setBusy(false);
    }
  }

  const inputSummary = Object.entries(request.tool_input)
    .slice(0, 2)
    .map(([k, v]) => `${k}: ${String(v).slice(0, 40)}`)
    .join(", ");

  if (responded === "timeout") {
    return (
      <div className="hitl-card hitl-card--resolved hitl-card--timeout">
        <span className="hitl-card__label">⏱ Timed out — request auto-denied</span>
      </div>
    );
  }
  if (responded === "allowed") {
    return (
      <div className="hitl-card hitl-card--resolved hitl-card--allowed">
        <span className="hitl-card__label">✓ Allowed</span>
      </div>
    );
  }
  if (responded === "denied") {
    return (
      <div className="hitl-card hitl-card--resolved hitl-card--denied">
        <span className="hitl-card__label">✕ Denied</span>
      </div>
    );
  }

  const mins = Math.floor(remaining / 60);
  const secs = remaining % 60;
  const countdown = `${mins}:${String(secs).padStart(2, "0")}`;

  return (
    <div className="hitl-card">
      <div className="hitl-card__header">
        <span className="hitl-card__icon">⏸</span>
        <span className="hitl-card__title">Approval needed</span>
        <span className="hitl-card__timer">{countdown}</span>
      </div>
      <div className="hitl-card__prompt">{request.prompt}</div>
      {inputSummary && (
        <div className="hitl-card__input">{inputSummary}</div>
      )}
      {showReasonInput ? (
        <div className="hitl-card__reason-form">
          <label className="hitl-card__reason-label">{t("hitl.denyReason")}</label>
          <textarea
            className="hitl-card__reason-input"
            placeholder={t("hitl.reasonPlaceholder")}
            value={reason}
            onChange={(e) => setReason(e.target.value)}
            rows={3}
            disabled={busy}
          />
          <div className="hitl-card__actions">
            <button
              className="hitl-card__btn hitl-card__btn--deny"
              onClick={() => respond(false, reason || undefined)}
              disabled={busy}
            >
              {t("hitl.confirmDeny")}
            </button>
            <button
              className="hitl-card__btn hitl-card__btn--cancel"
              onClick={() => { setShowReasonInput(false); setReason(""); }}
              disabled={busy}
            >
              Cancel
            </button>
          </div>
        </div>
      ) : (
        <div className="hitl-card__actions">
          <button
            className="hitl-card__btn hitl-card__btn--allow"
            onClick={() => respond(true)}
            disabled={busy}
          >
            Allow
          </button>
          <button
            className="hitl-card__btn hitl-card__btn--deny"
            onClick={() => respond(false)}
            disabled={busy}
          >
            Deny
          </button>
          <button
            className="hitl-card__btn hitl-card__btn--deny-reason"
            onClick={() => setShowReasonInput(true)}
            disabled={busy}
          >
            {t("hitl.denyWithReason")}
          </button>
        </div>
      )}
    </div>
  );
}
