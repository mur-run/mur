import { useT } from "../../i18n";

interface Props {
  onImportAgent: () => void;
  onImportPreset: () => void;
  onClose: () => void;
}

export function DataSettings({ onImportAgent, onImportPreset, onClose }: Props) {
  const { t } = useT();
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
    </section>
  );
}
