import { useEffect, useState } from "react";
import type { RenderProgressSnapshot, WizardSnapshot } from "../../../types";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useT } from "../../../i18n";

interface Props {
  snapshot: WizardSnapshot;
  onFinish: (agentName: string) => void;
}

export function Step6Render({ snapshot, onFinish }: Props) {
  const { t } = useT();
  const [progress, setProgress] = useState<RenderProgressSnapshot | null>(
    snapshot.render_progress ?? null,
  );
  const [done, setDone] = useState(snapshot.render_done);
  const [error, setError] = useState<string | null>(null);
  const [started, setStarted] = useState(false);

  useEffect(() => {
    const unsubProgress = listen<RenderProgressSnapshot>("wizard-render-progress", (ev) => {
      setProgress(ev.payload);
    });
    const unsubDone = listen<{ expressions_rendered: number; total: number }>(
      "wizard-render-done",
      () => setDone(true),
    );
    const unsubError = listen<string>("wizard-render-error", (ev) => {
      setError(ev.payload);
    });
    return () => {
      unsubProgress.then((f) => f());
      unsubDone.then((f) => f());
      unsubError.then((f) => f());
    };
  }, []);

  async function startRender() {
    setStarted(true);
    setError(null);
    try {
      await invoke("wizard_start_render");
    } catch (e) {
      setError(String(e));
      setStarted(false);
    }
  }

  async function finish() {
    try {
      const name: string = await invoke("wizard_finish");
      onFinish(name);
    } catch (e) {
      setError(String(e));
    }
  }

  async function useDefaultBlob() {
    try {
      const name: string = await invoke("wizard_finish");
      onFinish(name);
    } catch (e) {
      setError(String(e));
    }
  }

  const pct =
    progress && progress.total > 0
      ? Math.round((progress.done / progress.total) * 100)
      : 0;

  return (
    <div className="wz-step">
      <h2>{t("wizard.render.title")}</h2>
      <p className="wz-hint">{t("wizard.render.hint")}</p>

      {!started && !done && (
        <div className="wz-render-actions">
          <button className="btn btn--primary" onClick={startRender}>
            {t("wizard.render.generate")}
          </button>
          <button className="btn btn--secondary" onClick={useDefaultBlob}>
            {t("wizard.render.useDefault")}
          </button>
        </div>
      )}

      {started && !done && progress && (
        <div className="wz-progress">
          <div className="wz-progress-bar" style={{ width: `${pct}%` }} />
          <p className="wz-progress-text">
            {t("wizard.render.progress", { done: progress.done, total: progress.total })}
            {progress.failed > 0 && (
              <span className="wz-progress-failed">
                {" · "}
                {t("wizard.render.failed", { failed: progress.failed })}
              </span>
            )}
          </p>
        </div>
      )}

      {started && !done && !progress && (
        <p className="wz-progress-text">{t("wizard.render.starting")}</p>
      )}

      {done && (
        <div className="wz-render-done">
          <p>{t("wizard.render.complete")}</p>
          <button className="btn btn--primary" onClick={finish}>
            {t("wizard.render.finish")}
          </button>
        </div>
      )}

      {error && (
        <div className="wz-render-error">
          <p className="wz-error">{error}</p>
          <button className="btn btn--secondary" onClick={useDefaultBlob}>
            {t("wizard.render.continueDefault")}
          </button>
        </div>
      )}
    </div>
  );
}
