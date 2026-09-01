import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { useT } from "../../i18n";

interface Props {
  onImportAgent: () => void;
  onImportPreset: () => void;
  onClose: () => void;
}

export function DataSettings({ onImportAgent, onImportPreset, onClose }: Props) {
  const { t } = useT();
  // Three states, because the backend has three answers: it opened, it is
  // starting one, or it refused because something that is not MUR holds the
  // port. The starting state is visible so the click is not a frozen button.
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function openDashboard() {
    setStarting(true);
    setError(null);
    try {
      await invoke("dashboard_open");
    } catch (e) {
      setError(String(e));
    } finally {
      setStarting(false);
    }
  }

  return (
    <section className="settings-section">
      <h3 className="settings-section__title">{t("settings.nav.data")}</h3>
      <div className="settings-row">
        <button
          className="toolbar-btn"
          onClick={() => {
            onClose();
            onImportAgent();
          }}
          title={t("app.importAgentTooltip")}
        >
          {t("app.importAgent")}
        </button>
        <button
          className="toolbar-btn"
          onClick={() => {
            onClose();
            onImportPreset();
          }}
          title={t("app.importPresetTooltip")}
        >
          {t("app.importPreset")}
        </button>
      </div>
      <div className="settings-row">
        <button className="toolbar-btn" onClick={openDashboard} disabled={starting}>
          {starting
            ? t("settings.data.openDashboardStarting")
            : t("settings.data.openDashboard")}
        </button>
      </div>
      <div className="settings-hint">{t("settings.data.openDashboardHint")}</div>
      {error && <div className="settings-hint settings-hint--warn">{error}</div>}
    </section>
  );
}
