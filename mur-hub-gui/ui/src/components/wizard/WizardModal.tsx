import { useEffect, useReducer, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import type { TranslationKey } from "../../i18n/types";
import { Step0Source } from "./steps/Step0Source";
import { SpecRole } from "./steps/spec/SpecRole";
import { SpecGenerating } from "./steps/spec/SpecGenerating";
import type { SpecDraftDto } from "./steps/spec/SpecGenerating";
import { SpecReview } from "./steps/spec/SpecReview";
import { SpecEval } from "./steps/spec/SpecEval";
import { SpecAppearance } from "./steps/spec/SpecAppearance";
import { SpecOfficial } from "./steps/spec/SpecOfficial";
import { specReducer, SPEC_FLOW_INITIAL, type SpecFlowStep } from "./specFlow";

interface Props {
  isOpen: boolean;
  onClose: (agentName?: string) => void;
  /** Hand the "import a .muragent" source off to the host's import modal. */
  onImport: () => void;
}

/** Stepper labels for the template flow — the only multi-step source. */
const TEMPLATE_STEPS: { step: SpecFlowStep; labelKey: TranslationKey }[] = [
  { step: "role", labelKey: "wizard.step.role" },
  { step: "generating", labelKey: "wizard.step.generating" },
  { step: "review", labelKey: "wizard.step.review" },
  { step: "eval", labelKey: "wizard.step.eval" },
  { step: "appearance", labelKey: "wizard.step.appearance" },
];

export function WizardModal({ isOpen, onClose, onImport }: Props) {
  const { t } = useT();
  const [specFlow, dispatchSpec] = useReducer(specReducer, SPEC_FLOW_INITIAL);
  const [specRoleId, setSpecRoleId] = useState<string | null>(null);
  const [specNoLlm, setSpecNoLlm] = useState(false);
  const [specDraft, setSpecDraft] = useState<SpecDraftDto | null>(null);
  const [createdName, setCreatedName] = useState<string | null>(null);

  // Reset the flow every time the modal opens.
  useEffect(() => {
    if (isOpen) {
      dispatchSpec({ type: "RESET" });
      setSpecRoleId(null);
      setSpecNoLlm(false);
      setSpecDraft(null);
      setCreatedName(null);
    }
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
    // Clear any in-progress draft held in WizardSpecState so no stale state
    // lingers for the next open.
    if (specFlow.step !== "source") {
      invoke("wizard_spec_cancel").catch(() => {});
    }
    dispatchSpec({ type: "RESET" });
    onClose();
  }

  function finish(name: string | null) {
    dispatchSpec({ type: "RESET" });
    onClose(name ?? undefined);
  }

  if (!isOpen) return null;

  const stepIndex = TEMPLATE_STEPS.findIndex((s) => s.step === specFlow.step);
  const showStepper = stepIndex >= 0 && specFlow.source === "template";

  return (
    <div
      className="wz-overlay"
      onMouseDown={(e) => e.target === e.currentTarget && handleClose()}
    >
      <div className="wz-modal" role="dialog" aria-modal="true">
        <div className="wz-header">
          {showStepper && (
            <div className="wizard-stepper wizard-stepper--compact">
              {TEMPLATE_STEPS.map((s, i) => {
                const num = i + 1;
                const isDone = i < stepIndex;
                const isCurrent = i === stepIndex;
                return (
                  <div key={s.step} style={{ display: "flex", alignItems: "center" }}>
                    <div
                      className={`wizard-step${isDone ? " is-done" : ""}${
                        isCurrent ? " is-current" : ""
                      }`}
                    >
                      <span className="wizard-step__circle">{isDone ? "✓" : num}</span>
                      {isCurrent && (
                        <span className="wizard-step__label">{t(s.labelKey)}</span>
                      )}
                    </div>
                    {num < TEMPLATE_STEPS.length && (
                      <span className={`wizard-step__line${isDone ? " is-done" : ""}`} />
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
          {specFlow.step === "source" && (
            <Step0Source
              onSelect={(source) => {
                if (source === "import") {
                  // Nothing for the wizard to do — the host owns the import
                  // modal. Close first so two modals never stack.
                  dispatchSpec({ type: "RESET" });
                  onClose();
                  onImport();
                  return;
                }
                dispatchSpec({ type: "SELECT_SOURCE", source });
              }}
            />
          )}

          {specFlow.step === "role" && (
            <SpecRole
              onStart={(roleId, noLlm) => {
                setSpecRoleId(roleId);
                setSpecNoLlm(noLlm);
                dispatchSpec({ type: "NEXT" });
              }}
            />
          )}

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

          {specFlow.step === "review" && specDraft && (
            <SpecReview
              draft={specDraft}
              onCreated={(name) => {
                setCreatedName(name);
                dispatchSpec({ type: "NEXT" });
              }}
            />
          )}

          {specFlow.step === "eval" && createdName && (
            <SpecEval
              agentName={createdName}
              doneLabel={t("wizard.appearance.next")}
              onDone={() => dispatchSpec({ type: "NEXT" })}
            />
          )}

          {specFlow.step === "official" && (
            <SpecOfficial
              onInstalled={(name) => {
                // Fleets install several agents at once — there is no single
                // one to dress up, so those finish here.
                if (!name) {
                  finish(null);
                  return;
                }
                setCreatedName(name);
                dispatchSpec({ type: "NEXT" });
              }}
            />
          )}

          {/* Shared final step: any new agent can be given a pet look. */}
          {specFlow.step === "appearance" && createdName && (
            <SpecAppearance agentName={createdName} onDone={(name) => finish(name)} />
          )}
        </div>

        {/* Back is offered only where going back is safe — once the agent
            exists on disk there is nothing to go back to. */}
        {(specFlow.step === "role" ||
          specFlow.step === "official" ||
          specFlow.step === "generating" ||
          specFlow.step === "review") && (
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
