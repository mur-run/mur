import { useEffect, useState } from "react";
import type { WizardSnapshot } from "../../types";
import { invoke } from "@tauri-apps/api/core";
import { Step1Persona } from "./steps/Step1Persona";
import { Step2Name } from "./steps/Step2Name";
import { Step3Style } from "./steps/Step3Style";
import { Step4Behavior } from "./steps/Step4Behavior";
import { Step5Photo } from "./steps/Step5Photo";
import { Step6Render } from "./steps/Step6Render";

interface Props {
  isOpen: boolean;
  onClose: (agentName?: string) => void;
}

export function WizardModal({ isOpen, onClose }: Props) {
  const [snapshot, setSnapshot] = useState<WizardSnapshot | null>(null);
  const [displayStep, setDisplayStep] = useState(1);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!isOpen) return;
    setLoading(true);
    invoke<WizardSnapshot>("wizard_open")
      .then((s) => {
        setSnapshot(s);
        setDisplayStep(s.step);
      })
      .catch(console.error)
      .finally(() => setLoading(false));
  }, [isOpen]);

  useEffect(() => {
    if (!isOpen) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") handleClose();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [isOpen]);

  function handleClose() {
    invoke("wizard_cancel").catch(() => {});
    setSnapshot(null);
    setDisplayStep(1);
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

  const totalVisible = snapshot?.needs_photo ? 6 : 5;
  const visibleStep =
    !snapshot?.needs_photo && displayStep >= 6 ? 5 : displayStep;

  return (
    <div
      className="wz-overlay"
      onMouseDown={(e) => e.target === e.currentTarget && handleClose()}
    >
      <div className="wz-modal" role="dialog" aria-modal="true">
        <div className="wz-header">
          <div className="wz-progress-dots">
            {Array.from({ length: totalVisible }, (_, i) => (
              <span
                key={i}
                className={`wz-dot${
                  i + 1 === visibleStep
                    ? " wz-dot--active"
                    : i + 1 < visibleStep
                    ? " wz-dot--done"
                    : ""
                }`}
              />
            ))}
          </div>
          <button className="wz-close" onClick={handleClose} aria-label="Close">
            ✕
          </button>
        </div>

        <div className="wz-body">
          {loading || !snapshot ? (
            <div className="wz-loading">Loading…</div>
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
          )}
        </div>

        {!loading && snapshot && displayStep > 1 && displayStep < 6 && (
          <div className="wz-footer">
            <button className="wz-btn ghost" onClick={handleBack}>
              ← Back
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
