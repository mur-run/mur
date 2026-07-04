/** First-run model setup: one screen, three exits (apply / customize / skip).
 *  Shown only when model_setup_status says nothing usable is configured. */
import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../i18n";
import { ModelLibrary } from "./ModelLibrary";

export function ModelSetupWizard({
  open,
  onClose,
  onCustomize,
}: {
  open: boolean;
  onClose: () => void;
  onCustomize: () => void;
}) {
  const { t } = useT();
  const [summary, setSummary] = useState<string | null>(null);
  const [hasPlan, setHasPlan] = useState(false);
  const [phase, setPhase] = useState<"detect" | "ready" | "applying" | "done">("detect");
  const [libraryOpen, setLibraryOpen] = useState(false);

  const preview = useCallback(() => {
    setPhase("detect");
    invoke<{ summary: string; has_plan: boolean }>("model_setup_preview")
      .then((p) => {
        setSummary(p.summary);
        setHasPlan(p.has_plan);
        setPhase("ready");
      })
      .catch(() => {
        setSummary(null);
        setHasPlan(false);
        setPhase("ready");
      });
  }, []);
  useEffect(() => {
    if (open) preview();
  }, [open, preview]);
  useEffect(() => {
    if (!libraryOpen && open) preview();
  }, [libraryOpen]); // re-probe after connecting

  if (!open) return null;
  return (
    <div className="wizard-overlay" role="dialog" aria-modal="true" aria-label={t("wizard.models.title")}>
      <div className="wizard-card">
        <h2>{t("wizard.models.title")}</h2>
        {phase === "detect" && <p>{t("wizard.models.detecting")}</p>}
        {phase !== "detect" && <p className="wizard-summary">{summary}</p>}
        {phase === "done" ? (
          <>
            <p>
              {t("wizard.models.done")} {summary}
            </p>
            <button className="toolbar-btn" onClick={onClose}>
              OK
            </button>
          </>
        ) : (
          <div className="wizard-actions">
            {hasPlan ? (
              <button
                className="toolbar-btn toolbar-btn--primary"
                disabled={phase !== "ready"}
                onClick={() => {
                  setPhase("applying");
                  invoke<{ summary: string }>("model_setup_apply_recommended")
                    .then((p) => {
                      setSummary(p.summary);
                      setPhase("done");
                    })
                    .catch(() => setPhase("ready"));
                }}
              >
                {t("wizard.models.apply")}
              </button>
            ) : (
              <button className="toolbar-btn toolbar-btn--primary" onClick={() => setLibraryOpen(true)}>
                {t("wizard.models.connect")}
              </button>
            )}
            <button
              className="toolbar-btn"
              onClick={() => {
                onClose();
                onCustomize();
              }}
            >
              {t("wizard.models.customize")}
            </button>
            <button className="toolbar-btn" onClick={onClose}>
              {t("wizard.models.skip")}
            </button>
          </div>
        )}
        <p className="settings-hint">{t("wizard.models.hint")}</p>
        <ModelLibrary open={libraryOpen} onClose={() => setLibraryOpen(false)} />
      </div>
    </div>
  );
}
