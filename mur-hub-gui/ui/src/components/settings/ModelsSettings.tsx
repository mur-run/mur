import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import { ModelLibrary } from "../ModelLibrary";
import { RegistryList } from "../ModelLibraryPanels";
import type { ModelOption } from "../modelPicker";

export function ModelsSettings() {
  const { t } = useT();
  const [model, setModel] = useState<string | null>(null);
  const [libraryOpen, setLibraryOpen] = useState(false);
  const [models, setModels] = useState<ModelOption[]>([]);

  const refreshModels = useCallback(() => {
    invoke<ModelOption[]>("list_models")
      .then(setModels)
      .catch(() => {});
  }, []);

  useEffect(() => {
    invoke<[boolean, string | null]>("nudge_status")
      .then(([, m]) => setModel(m))
      .catch(() => {});
    refreshModels();
  }, [refreshModels]);

  // Library writes the same ~/.mur/models.yaml — re-pull the list once it closes.
  useEffect(() => {
    if (!libraryOpen) refreshModels();
  }, [libraryOpen, refreshModels]);

  return (
    <section className="settings-section">
      <h3 className="settings-section__title">{t("settings.nav.models")}</h3>
      <div className="settings-row">
        <span className="settings-row__label">{t("settings.defaultBrain")}</span>
        <span className="settings-row__value">
          {model ? `🧠 ${model}` : t("settings.noBrain")}
        </span>
      </div>

      {models.length > 0 ? (
        <RegistryList models={models} onChanged={refreshModels} />
      ) : (
        <p className="settings-hint">{t("settings.noModels")}</p>
      )}

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
