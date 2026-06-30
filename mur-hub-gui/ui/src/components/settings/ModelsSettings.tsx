import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import { ModelLibrary } from "../ModelLibrary";

export function ModelsSettings() {
  const { t } = useT();
  const [model, setModel] = useState<string | null>(null);
  const [libraryOpen, setLibraryOpen] = useState(false);

  useEffect(() => {
    invoke<[boolean, string | null]>("nudge_status")
      .then(([, m]) => setModel(m))
      .catch(() => {});
  }, []);

  return (
    <section className="settings-section">
      <h3 className="settings-section__title">{t("settings.nav.models")}</h3>
      <div className="settings-row">
        <span className="settings-row__label">{t("settings.defaultBrain")}</span>
        <span className="settings-row__value">
          {model ? `🧠 ${model}` : t("settings.noBrain")}
        </span>
      </div>
      <div className="settings-row">
        <button className="toolbar-btn" onClick={() => setLibraryOpen(true)}>
          {t("settings.openLibrary")}
        </button>
      </div>
      <p className="settings-hint">{t("settings.modelsHint")}</p>
      <ModelLibrary open={libraryOpen} onClose={() => setLibraryOpen(false)} />
    </section>
  );
}
