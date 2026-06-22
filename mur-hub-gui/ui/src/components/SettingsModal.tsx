import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../i18n";

interface Props {
  isOpen: boolean;
  onClose: () => void;
  /** Open the .muragent import flow (owned by DashboardApp). */
  onImportAgent: () => void;
  /** Open the style-preset import flow (owned by DashboardApp). */
  onImportPreset: () => void;
}

/**
 * Hub-level settings. Consolidates the global controls that used to clutter the
 * top bar (language, brain/model, imports) into one panel so the header can be
 * about navigation + the primary action.
 */
export function SettingsModal({
  isOpen,
  onClose,
  onImportAgent,
  onImportPreset,
}: Props) {
  const { t, lang, setLang } = useT();
  const [model, setModel] = useState<string | null>(null);

  useEffect(() => {
    if (!isOpen) return;
    invoke<[boolean, string | null]>("nudge_status")
      .then(([, m]) => setModel(m))
      .catch(() => {});
  }, [isOpen]);

  if (!isOpen) return null;

  return (
    <div className="modal__overlay" onClick={onClose}>
      <div
        className="modal settings-modal"
        style={{ width: 460 }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal__header">
          <h2 className="modal__title">{t("settings.title")}</h2>
          <button className="modal__close" onClick={onClose}>
            ×
          </button>
        </div>

        <div className="modal__body">
          {/* ── Appearance ── */}
          <section className="settings-section">
            <h3 className="settings-section__title">
              {t("settings.section.appearance")}
            </h3>
            <div className="settings-row">
              <label className="settings-row__label" htmlFor="settings-lang">
                {t("settings.language")}
              </label>
              <select
                id="settings-lang"
                className="input"
                value={lang}
                onChange={(e) => setLang(e.target.value as "en" | "zh-TW")}
              >
                <option value="en">English</option>
                <option value="zh-TW">繁體中文</option>
              </select>
            </div>
          </section>

          {/* ── Models ── */}
          <section className="settings-section">
            <h3 className="settings-section__title">
              {t("settings.section.models")}
            </h3>
            <div className="settings-row">
              <span className="settings-row__label">
                {t("settings.defaultBrain")}
              </span>
              <span className="settings-row__value">
                {model ? `🧠 ${model}` : t("settings.noBrain")}
              </span>
            </div>
            <p className="settings-hint">{t("settings.modelsHint")}</p>
          </section>

          {/* ── Import ── */}
          <section className="settings-section">
            <h3 className="settings-section__title">
              {t("settings.section.import")}
            </h3>
            <div className="settings-actions">
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
        </div>
      </div>
    </div>
  );
}
