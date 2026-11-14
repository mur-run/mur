//! The three-step SSH setup card (SSH setup spec, "The card"), as a modal.
//! Every decision is the backend's: Approve is enabled only when
//! `agent_ssh_check` returns `ready`.

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import { newTaskId } from "../ChatTab";
import { type ApplyView, type CheckView, type Choices, type PlanView, sshApi } from "./sshApi";
import { initialChoices, notReadyText, verdictKey } from "./sshModel";

type Step = "key" | "host" | "summary" | "done";

export interface SshTarget {
  agent: string;
  host: string;
  port: number | null;
  user: string | null;
}

export function SshCard({ target, onClose }: { target: SshTarget; onClose: (changed: boolean) => void }) {
  const { t } = useT();
  const [plan, setPlan] = useState<PlanView | null>(null);
  const [choices, setChoices] = useState<Choices | null>(null);
  const [step, setStep] = useState<Step>("key");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [check, setCheck] = useState<CheckView | null>(null);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<ApplyView | null>(null);
  const [note, setNote] = useState<string | null>(null);

  function load() {
    sshApi
      .plan(target.agent, target.host, target.port, target.user)
      .then((p) => {
        setPlan(p);
        setChoices(initialChoices(p));
        setStep("key");
        setError(null);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  useEffect(() => {
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target.agent, target.host, target.port, target.user]);

  useEffect(() => {
    if (!plan || !choices || step !== "summary") return;
    let alive = true;
    sshApi
      .check(plan.plan_id, choices)
      .then((c) => alive && setCheck(c))
      .catch((e) => alive && setError(String(e)));
    return () => {
      alive = false;
    };
  }, [plan, choices, step]);

  function retry() {
    setLoading(true);
    load();
  }

  function recheck() {
    if (!plan) return;
    sshApi.refreshKeys(plan.plan_id).then(setPlan).catch((e) => setError(String(e)));
  }

  function toggle(fp: string) {
    if (!choices) return;
    const ticked = choices.ticked.includes(fp) ? choices.ticked.filter((x) => x !== fp) : [...choices.ticked, fp];
    setChoices({ ...choices, ticked });
  }

  async function approve() {
    if (!plan || !choices) return;
    setBusy(true);
    setError(null);
    try {
      const r = await sshApi.apply(plan.plan_id, choices);
      setResult(r);
      setStep("done");
      if (r.hand_back) {
        setNote(t("ssh.done.asking", { agent: plan.agent }));
        invoke("agent_chat_send", {
          name: plan.agent,
          text: r.hand_back,
          taskId: newTaskId(),
          contextTaskId: null,
          channelId: null,
        })
          .then(() => setNote(t("ssh.done.handedBack", { agent: plan.agent })))
          .catch((e) => setNote(t("ssh.done.handBackFailed", { agent: plan.agent, err: String(e) })));
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const chosen = plan && choices ? plan.keys.find((k) => k.fingerprint === choices.key_fp) ?? null : null;

  return (
    <div className="modal__overlay">
      <div className="modal ssh-card" role="dialog" aria-modal="true">
        <div className="modal__header">
          <h2 className="modal__title">
            {t("ssh.card.title", { agent: target.agent, host: plan?.host ?? target.host })}
          </h2>
        </div>
        <div className="modal__body">
          {plan?.reason && (
            <>
              <p className="field-muted">{t("ssh.card.reasonIntro")}</p>
              <blockquote className="ssh-reason">{plan.reason}</blockquote>
            </>
          )}
          {loading && <p className="field-muted">{t("ssh.card.scanning")}</p>}
          {error && <p className="save-error">{error}</p>}
          {plan && choices && step !== "done" && (
            <p className="ssh-card__steps">
              {(["key", "host", "summary"] as const).map((s) => (
                <span key={s} className={s === step ? "ssh-card__step ssh-card__step--on" : "ssh-card__step"}>
                  {t(`ssh.step.${s}`)}
                </span>
              ))}
            </p>
          )}

          {plan && choices && step === "key" && (
            plan.keys.length === 0 ? (
              <p className="field-muted">{t("ssh.key.none")}</p>
            ) : (
              <ul className="perm__list">
                {plan.keys.map((k) => (
                  <li key={k.fingerprint} className="ssh-card__key">
                    <label className="perm__row">
                      <input
                        type="radio"
                        name="ssh-key"
                        checked={choices.key_fp === k.fingerprint}
                        onChange={() => setChoices({ ...choices, key_fp: k.fingerprint })}
                      />
                      <code>{k.fingerprint}</code>
                      <span className="perm__detail">{k.comment}</span>
                      <span className="badge-sm">{k.loaded ? t("ssh.key.loaded") : t("ssh.key.notLoaded")}</span>
                    </label>
                    {!k.loaded && k.add_cmd && choices.key_fp === k.fingerprint && (
                      <div className="ssh-card__hint">
                        <p className="field-muted">{t("ssh.key.loadHint")}</p>
                        <code className="ssh-card__cmd">{k.add_cmd}</code>
                        <button type="button" className="btn btn--sm btn--secondary" onClick={recheck}>
                          {t("ssh.key.recheck")}
                        </button>
                      </div>
                    )}
                  </li>
                ))}
              </ul>
            )
          )}

          {plan && choices && step === "host" && (
            <>
              <p className={`ssh-card__verdict ssh-card__verdict--${plan.verdict}`}>{t(verdictKey(plan.verdict))}</p>
              <ul className="perm__list">
                {plan.scanned.map((k) => (
                  <li key={k.fingerprint} className="perm__row">
                    {plan.ticks_offered ? (
                      <label>
                        <input type="checkbox" checked={choices.ticked.includes(k.fingerprint)} onChange={() => toggle(k.fingerprint)} />{" "}
                        {t("ssh.host.compared")}
                      </label>
                    ) : (
                      <span className="perm__glyph">{k.matched ? "✓" : ""}</span>
                    )}
                    <code>
                      {k.key_type} {k.fingerprint}
                    </code>
                  </li>
                ))}
              </ul>
              {plan.verdict !== "trusted" && (
                <input
                  className="input ssh-card__paste"
                  placeholder={t("ssh.host.paste")}
                  value={choices.pasted ?? ""}
                  onChange={(e) => setChoices({ ...choices, pasted: e.target.value.trim() || null })}
                />
              )}
              {plan.differs_cmd && (
                <p className="field-muted">
                  {t("ssh.host.differsExit")} <code>{plan.differs_cmd}</code>
                </p>
              )}
            </>
          )}

          {plan && choices && step === "summary" && (
            <>
              <label className="field-label">{t("ssh.summary.user")}</label>
              <input className="input" value={choices.user} onChange={(e) => setChoices({ ...choices, user: e.target.value.trim() })} />
              <p className="field-label">{t("ssh.summary.writes")}</p>
              {check?.ready && (
                <ul className="perm__list">
                  <li className="perm__row"><code>{t("ssh.summary.key", { fp: check.ready.key_fp })}</code></li>
                  {check.ready.host_keys.map((h) => (
                    <li key={h} className="perm__row"><code>{t("ssh.summary.hostKey", { fp: h })}</code></li>
                  ))}
                  {check.ready.spawn === "add_ssh" && <li className="perm__row">{t("ssh.summary.addSsh")}</li>}
                  <li className="perm__row">{t("ssh.summary.restart", { agent: plan.agent })}</li>
                </ul>
              )}
              {check?.not_ready && <p className="save-error">{notReadyText(check.not_ready, t)}</p>}
              {plan.spawn === "none_to_strict" && (
                <div className="ssh-card__consent">
                  <label>
                    <input
                      type="checkbox"
                      checked={choices.spawn_consented}
                      onChange={() => setChoices({ ...choices, spawn_consented: !choices.spawn_consented })}
                    />{" "}
                    {t("ssh.summary.consent")}
                  </label>
                  {plan.consent_text && <pre className="ssh-card__surface">{plan.consent_text}</pre>}
                  <p className="field-muted">{t("ssh.summary.consentAlt")}</p>
                </div>
              )}
            </>
          )}

          {step === "done" && result && (
            <>
              <ul className="perm__list">
                {result.lines.map((l) => (
                  <li key={l} className="perm__row">{l}</li>
                ))}
              </ul>
              {note && <p className="field-muted">{note}</p>}
            </>
          )}
        </div>
        <div className="modal__footer ssh-card__footer">
          {error && !plan && (
            <button type="button" className="btn btn--sm btn--secondary" onClick={retry}>
              {t("ssh.card.retry")}
            </button>
          )}
          {step === "done" ? (
            <>
              {result?.can_restart && plan && (
                <button
                  type="button"
                  className="btn btn--sm btn--secondary"
                  onClick={() => void invoke("restart_agent", { name: plan.agent }).catch((e) => setError(String(e)))}
                >
                  {t("ssh.done.restart", { agent: plan.agent })}
                </button>
              )}
              <button type="button" className="btn btn--sm btn--primary" onClick={() => onClose(true)}>
                {t("ssh.close")}
              </button>
            </>
          ) : (
            <>
              <button type="button" className="btn btn--sm btn--secondary" onClick={() => onClose(false)} disabled={busy}>
                {t("ssh.cancel")}
              </button>
              {step !== "key" && (
                <button
                  type="button"
                  className="btn btn--sm btn--secondary"
                  onClick={() => setStep(step === "summary" ? "host" : "key")}
                  disabled={busy}
                >
                  {t("ssh.back")}
                </button>
              )}
              {step === "key" && (
                <button type="button" className="btn btn--sm btn--primary" onClick={() => setStep("host")} disabled={!chosen?.loaded}>
                  {t("ssh.next")}
                </button>
              )}
              {step === "host" && (
                <button type="button" className="btn btn--sm btn--primary" onClick={() => setStep("summary")}>
                  {t("ssh.next")}
                </button>
              )}
              {step === "summary" && (
                <button type="button" className="btn btn--sm btn--primary" onClick={() => void approve()} disabled={busy || !check?.ready}>
                  {t("ssh.approve")}
                </button>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
