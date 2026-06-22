import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { useT } from "../../../../i18n";

/** Scores for a single eval task (mirrors mur-core DimensionScores). */
interface DimensionScores {
  correctness: number;
  honesty: number;
  uses_skills: number;
  safety_ok: boolean;
}

/** Single task result (mirrors mur-core EvalResult). */
interface EvalResult {
  task_id: string;
  scores: DimensionScores;
  response: string;
}

/** Full eval report (mirrors mur-core EvalReport). */
interface EvalReport {
  passed: boolean;
  results: EvalResult[];
}

interface Props {
  agentName: string;
  /** Called to close the wizard after the user dismisses eval results. */
  onDone: (agentName: string) => void;
  /** Override the primary button label (e.g. "Next: appearance" in the Both flow). */
  doneLabel?: string;
}

export function SpecEval({ agentName, onDone, doneLabel }: Props) {
  const { t } = useT();
  const [report, setReport] = useState<EvalReport | null>(null);

  // Register listener at mount so we don't miss early events.
  useEffect(() => {
    const unlisten = listen<EvalReport>("wizard-spec-eval", (ev) => {
      setReport(ev.payload);
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  function scoreBar(value: number) {
    // value is 1–5; map to percentage
    const pct = Math.round(((value - 1) / 4) * 100);
    const color =
      value >= 4 ? "var(--color-success, #22c55e)" :
      value >= 3 ? "var(--color-warn, #f59e0b)" :
                   "var(--color-error, #ef4444)";
    return (
      <span
        className="wz-eval-bar"
        style={{
          display: "inline-block",
          width: `${pct}%`,
          minWidth: "4px",
          height: "8px",
          background: color,
          borderRadius: "4px",
        }}
      />
    );
  }

  if (!report) {
    return (
      <div className="wz-step wz-eval">
        <h2>{t("wizard.eval.title")}</h2>
        <p className="wz-hint">{t("wizard.eval.running")}</p>
        <div className="wz-spec-log">
          <p className="wz-progress-text">{t("wizard.eval.wait")}</p>
        </div>
        <div className="wz-role-actions">
          {/* Allow users to skip waiting and go straight to the agent */}
          <button
            className="btn btn--secondary"
            onClick={() => onDone(agentName)}
          >
            {t("wizard.eval.skipWait")}
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="wz-step wz-eval">
      <h2>{t("wizard.eval.title")}</h2>

      <div
        className={`wz-eval-verdict ${report.passed ? "wz-eval-verdict--pass" : "wz-eval-verdict--fail"}`}
      >
        {report.passed ? t("wizard.eval.passed") : t("wizard.eval.failed")}
      </div>

      {report.results.length === 0 && (
        <p className="wz-hint">{t("wizard.eval.noResults")}</p>
      )}

      {report.results.length > 0 && (
        <div className="wz-eval-results">
          {report.results.map((r) => (
            <details key={r.task_id} className="wz-eval-task">
              <summary className="wz-eval-task-summary">
                <span className="wz-eval-task-id">{r.task_id}</span>
                <span className="wz-eval-scores">
                  <span className="wz-eval-score-label">
                    {t("wizard.eval.correctness")} {r.scores.correctness}/5
                  </span>
                  {scoreBar(r.scores.correctness)}
                  {"  "}
                  <span className="wz-eval-score-label">
                    {t("wizard.eval.honesty")} {r.scores.honesty}/5
                  </span>
                  {scoreBar(r.scores.honesty)}
                  {"  "}
                  <span className="wz-eval-score-label">
                    {t("wizard.eval.usesSkills")} {r.scores.uses_skills}/5
                  </span>
                  {scoreBar(r.scores.uses_skills)}
                  {"  "}
                  <span
                    className={`wz-eval-safety ${r.scores.safety_ok ? "wz-eval-safety--ok" : "wz-eval-safety--fail"}`}
                  >
                    {r.scores.safety_ok
                      ? t("wizard.eval.safetyOk")
                      : t("wizard.eval.safetyFail")}
                  </span>
                </span>
              </summary>
              <p className="wz-eval-response">{r.response}</p>
            </details>
          ))}
        </div>
      )}

      <div className="wz-role-actions">
        <button
          className="btn btn--primary"
          onClick={() => onDone(agentName)}
        >
          {doneLabel ?? t("wizard.eval.finish")}
        </button>
      </div>
    </div>
  );
}
