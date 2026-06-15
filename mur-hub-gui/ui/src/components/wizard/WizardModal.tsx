import { useEffect, useReducer, useState } from "react";
import type { WizardSnapshot } from "../../types";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import { Step0Kind } from "./steps/Step0Kind";
import { Step1Persona } from "./steps/Step1Persona";
import { Step2Name } from "./steps/Step2Name";
import { Step3Style } from "./steps/Step3Style";
import { Step4Behavior } from "./steps/Step4Behavior";
import { Step5Photo } from "./steps/Step5Photo";
import { Step6Render } from "./steps/Step6Render";
import { SpecRole } from "./steps/spec/SpecRole";
import { SpecGenerating } from "./steps/spec/SpecGenerating";
import type { SpecDraftDto } from "./steps/spec/SpecGenerating";
import { SpecReview } from "./steps/spec/SpecReview";
import { SpecEval } from "./steps/spec/SpecEval";
import { specReducer, SPEC_FLOW_INITIAL } from "./specFlow";

interface Props {
  isOpen: boolean;
  onClose: (agentName?: string) => void;
}

const STEP_LABEL_KEYS = [
  "wizard.step.persona",
  "wizard.step.name",
  "wizard.step.style",
  "wizard.step.behavior",
  "wizard.step.photo",
  "wizard.step.render",
] as const;

export function WizardModal({ isOpen, onClose }: Props) {
  const { t } = useT();
  const [snapshot, setSnapshot] = useState<WizardSnapshot | null>(null);
  const [displayStep, setDisplayStep] = useState(1);
  const [loading, setLoading] = useState(false);
  const [specFlow, dispatchSpec] = useReducer(specReducer, SPEC_FLOW_INITIAL);
  // Specialist flow state
  const [specRoleId, setSpecRoleId] = useState<string | null>(null);
  const [specNoLlm, setSpecNoLlm] = useState(false);
  const [specDraft, setSpecDraft] = useState<SpecDraftDto | null>(null);
  const [specCreatedName, setSpecCreatedName] = useState<string | null>(null);

  // Reset the kind-fork state every time the modal opens.
  useEffect(() => {
    if (isOpen) {
      dispatchSpec({ type: "RESET" });
      setSpecRoleId(null);
      setSpecNoLlm(false);
      setSpecDraft(null);
      setSpecCreatedName(null);
    }
  }, [isOpen]);

  // Only invoke wizard_open once the user has picked "companion" (or "both" in the
  // future when that also opens the companion wizard). For "specialist" the backend
  // call is issued by the specialist steps (T5+).
  useEffect(() => {
    if (!isOpen) return;
    if (specFlow.step !== "companion") return;
    setLoading(true);
    invoke<WizardSnapshot>("wizard_open")
      .then((s) => {
        setSnapshot(s);
        setDisplayStep(s.step);
      })
      .catch(console.error)
      .finally(() => setLoading(false));
  }, [isOpen, specFlow.step]);

  useEffect(() => {
    if (!isOpen) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") handleClose();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [isOpen]);

  function handleClose() {
    // Cancel whichever backend session was started so no stale state lingers.
    if (specFlow.step === "companion") {
      invoke("wizard_cancel").catch(() => {});
    } else if (specFlow.step !== "kind") {
      // Specialist/both path: clear any in-progress draft held in WizardSpecState.
      invoke("wizard_spec_cancel").catch(() => {});
    }
    setSnapshot(null);
    setDisplayStep(1);
    dispatchSpec({ type: "RESET" });
    onClose();
  }

  function handleUpdate(s: WizardSnapshot) {
    setSnapshot(s);
    const nextStep = s.step === 5 && !s.needs_photo ? 6 : s.step;
    setDisplayStep(nextStep);
  }

  function handleBack() {
    if (displayStep <= 1) return;
    let prev = displayStep - 1;
    if (prev === 5 && snapshot && !snapshot.needs_photo) prev = 4;
    setDisplayStep(prev);
  }

  function handleFinish(agentName: string) {
    setSnapshot(null);
    setDisplayStep(1);
    onClose(agentName);
  }

  if (!isOpen) return null;

  const needsPhoto = snapshot?.needs_photo ?? true;
  // Visible step labels: skip the Photo step when not needed.
  const labelKeys = needsPhoto
    ? STEP_LABEL_KEYS
    : STEP_LABEL_KEYS.filter((k) => k !== "wizard.step.photo");
  const totalVisible = labelKeys.length;
  const rawVisibleStep =
    !needsPhoto && displayStep >= 6 ? 5 : displayStep;
  // When photo is skipped, the render step (displayStep 6 → rawVisibleStep 5)
  // maps onto the 5th visible slot, which is correct.
  const visibleStep = rawVisibleStep;

  return (
    <div
      className="wz-overlay"
      onMouseDown={(e) => e.target === e.currentTarget && handleClose()}
    >
      <div className="wz-modal" role="dialog" aria-modal="true">
        <div className="wz-header">
          {/* Hide the step-progress stepper on Step 0 (kind fork). */}
          {specFlow.step !== "kind" && (
            <div className="wizard-stepper wizard-stepper--compact">
              {labelKeys.map((key, i) => {
                const num = i + 1;
                const isDone = num < visibleStep;
                const isCurrent = num === visibleStep;
                return (
                  <div key={key} style={{ display: "flex", alignItems: "center" }}>
                    <div
                      className={`wizard-step${isDone ? " is-done" : ""}${
                        isCurrent ? " is-current" : ""
                      }`}
                    >
                      <span className="wizard-step__circle">
                        {isDone ? "✓" : num}
                      </span>
                      {isCurrent && (
                        <span className="wizard-step__label">{t(key)}</span>
                      )}
                    </div>
                    {num < totalVisible && (
                      <span
                        className={`wizard-step__line${isDone ? " is-done" : ""}`}
                      />
                    )}
                  </div>
                );
              })}
            </div>
          )}
          <button
            className="wz-close"
            onClick={handleClose}
            aria-label={t("wizard.close")}
          >
            ✕
          </button>
        </div>

        <div className="wz-body">
          {/* ── Step 0: kind fork (always client-side, no backend call yet) ── */}
          {specFlow.step === "kind" && (
            <Step0Kind
              onSelect={(kind) => dispatchSpec({ type: "SELECT_KIND", kind })}
            />
          )}

          {/* ── Companion flow (Steps 1–6 unchanged) ── */}
          {specFlow.step === "companion" && (
            loading || !snapshot ? (
              <div className="wz-loading">{t("wizard.loading")}</div>
            ) : (
              <>
                {displayStep === 1 && (
                  <Step1Persona snapshot={snapshot} onUpdate={handleUpdate} />
                )}
                {displayStep === 2 && (
                  <Step2Name snapshot={snapshot} onUpdate={handleUpdate} />
                )}
                {displayStep === 3 && (
                  <Step3Style snapshot={snapshot} onUpdate={handleUpdate} />
                )}
                {displayStep === 4 && (
                  <Step4Behavior snapshot={snapshot} onUpdate={handleUpdate} />
                )}
                {displayStep === 5 && snapshot.needs_photo && (
                  <Step5Photo
                    snapshot={snapshot}
                    onUpdate={handleUpdate}
                    onSkip={() => setDisplayStep(6)}
                  />
                )}
                {displayStep === 6 && (
                  <Step6Render snapshot={snapshot} onFinish={handleFinish} />
                )}
              </>
            )
          )}

          {/* ── Specialist flow — Role picker (T5) ── */}
          {specFlow.step === "role" && (
            <SpecRole
              onStart={(roleId, noLlm) => {
                setSpecRoleId(roleId);
                setSpecNoLlm(noLlm);
                dispatchSpec({ type: "NEXT" });
              }}
            />
          )}

          {/* ── Specialist flow — Generating (T5) ── */}
          {specFlow.step === "generating" && specRoleId && (
            <SpecGenerating
              roleId={specRoleId}
              noLlm={specNoLlm}
              onDraft={(draft) => {
                setSpecDraft(draft);
                dispatchSpec({ type: "NEXT" });
              }}
            />
          )}

          {/* ── Specialist flow — Review (T6): editable gate ── */}
          {specFlow.step === "review" && specDraft && (
            <SpecReview
              draft={specDraft}
              onCreated={(name) => {
                setSpecCreatedName(name);
                dispatchSpec({ type: "NEXT" });
              }}
            />
          )}

          {/* ── Specialist flow — Eval (T6): results display ── */}
          {specFlow.step === "eval" && specCreatedName && (
            <SpecEval
              agentName={specCreatedName}
              onDone={(name) => {
                setSnapshot(null);
                setDisplayStep(1);
                onClose(name);
              }}
            />
          )}
        </div>

        {/* Back button: show in companion steps 2–5 and when returning from companion/specialist to kind */}
        {specFlow.step === "kind" ? null : specFlow.step === "companion" ? (
          !loading && snapshot && displayStep > 1 && displayStep < 6 && (
            <div className="wz-footer">
              <button className="btn btn--secondary" onClick={handleBack}>
                ← {t("wizard.back")}
              </button>
            </div>
          )
        ) : (
          <div className="wz-footer">
            <button
              className="btn btn--secondary"
              onClick={() => dispatchSpec({ type: "BACK" })}
            >
              ← {t("wizard.back")}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
