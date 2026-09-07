import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { HitlRequest, PermissionsView } from "../types";
import { useT } from "../i18n";
import { alwaysRuleFor, grantHintFor } from "./hitlModel";

interface Props {
  request: HitlRequest;
  /** P1 permissions view, for the grant hint. `null` = unknown, no hint. */
  perms: PermissionsView | null;
  /** Whether a profile write needs a restart to land (it always does while running). */
  isRunning: boolean;
}

export function HitlCard({ request, perms, isRunning }: Props) {
  const { t } = useT();
  const always = alwaysRuleFor(request.tool_name);
  const grant = grantHintFor(request.tool_name, request.tool_input, perms);
  const [ruleWritten, setRuleWritten] = useState(false);
  const [granted, setGranted] = useState(false);
  const [error, setError] = useState<string | null>(null);
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

  // "Always" = the one-time allow (releases the open gate now) THEN the exact
  // rule (takes effect on restart). Never the other way round: a rule write
  // cannot release a gate that is open now.
  async function respondAlways() {
    if (!always || responded || busy) return;
    await respond(true);
    try {
      await invoke("agent_perm_set_tool", {
        name: request.agent,
        pattern: always.pattern,
        policy: always.policy,
      });
      setRuleWritten(true);
    } catch (e) {
      setError(String(e));
    }
  }

  // The grant is a separate control: it changes what the agent can reach,
  // not whether this call may run. Never bundled into "always".
  async function doGrant() {
    if (!grant || busy) return;
    setBusy(true);
    try {
      await invoke("agent_perm_grant_path", { name: request.agent, verb: grant.verb, path: grant.path });
      setGranted(true);
      setRuleWritten(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const restartHint = ruleWritten ? (
    <div className="hitl-card__hint">{t(isRunning ? "perm.restartHint" : "perm.saved")}</div>
  ) : null;

  function toolLabel(name: string): string {
    const m = name.match(/^mcp__([^_]+(?:_[^_]+)*)__(.+)$/);
    if (m) return `${m[1].replace(/_/g, "-")} · ${m[2]}`;
    return name;
  }

  const hasArgs = Object.keys(request.tool_input).length > 0;

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
        {restartHint}
        {error && <div className="hitl-card__error">{error}</div>}
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
      <div className="hitl-card__tool">{toolLabel(request.tool_name)}</div>
      <div className="hitl-card__prompt">{request.prompt}</div>
      {hasArgs && (
        <pre className="hitl-args">{JSON.stringify(request.tool_input, null, 2)}</pre>
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
            {t("hitl.allowOnce")}
          </button>
          {always && (
            <button
              className="hitl-card__btn hitl-card__btn--always"
              title={t("hitl.alwaysHint", { tool: request.tool_name })}
              onClick={() => respondAlways()}
              disabled={busy}
            >
              {t("hitl.always")}
            </button>
          )}
          <button
            className="hitl-card__btn hitl-card__btn--deny"
            onClick={() => respond(false)}
            disabled={busy}
          >
            {t("hitl.deny")}
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
      {grant && (
        <div className="hitl-card__grant">
          <span>{t("hitl.grantHint")}</span>
          <button className="hitl-card__btn" onClick={() => doGrant()} disabled={busy || granted}>
            {granted ? "✓" : t("hitl.grant", { verb: grant.verb, path: grant.path })}
          </button>
        </div>
      )}
      {restartHint}
      {error && <div className="hitl-card__error">{error}</div>}
    </div>
  );
}
