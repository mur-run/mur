import { useState } from "react";
import { useT } from "../i18n";
import type { TranslationKey } from "../i18n/types";
import { GeneralSettings } from "./settings/GeneralSettings";
import { ModelsSettings } from "./settings/ModelsSettings";
import { UpdatesSettings } from "./settings/UpdatesSettings";
import { DataSettings } from "./settings/DataSettings";
import { AboutSettings } from "./settings/AboutSettings";

interface Props {
  isOpen: boolean;
  onClose: () => void;
  onImportAgent: () => void;
  onImportPreset: () => void;
}

type SectionId = "general" | "models" | "updates" | "data" | "about";

const NAV: { id: SectionId; labelKey: TranslationKey; icon: string }[] = [
  { id: "general", labelKey: "settings.nav.general", icon: "⚙️" },
  { id: "models", labelKey: "settings.nav.models", icon: "🧠" },
  { id: "updates", labelKey: "settings.nav.updates", icon: "⬆️" },
  { id: "data", labelKey: "settings.nav.data", icon: "📦" },
  { id: "about", labelKey: "settings.nav.about", icon: "ℹ️" },
];

export function SettingsModal({
  isOpen,
  onClose,
  onImportAgent,
  onImportPreset,
}: Props) {
  const { t } = useT();
  const [active, setActive] = useState<SectionId>("general");

  if (!isOpen) return null;

  return (
    <div className="modal__overlay" onClick={onClose}>
      <div
        className="modal settings-modal settings-modal--paned"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal__header">
          <h2 className="modal__title">{t("settings.title")}</h2>
          <button className="modal__close" onClick={onClose}>
            ×
          </button>
        </div>

        <div className="settings-modal__panes">
          <nav className="sidebar settings-nav">
            {NAV.map((n) => (
              <button
                key={n.id}
                className={`sidebar-item${active === n.id ? " sidebar-item--active" : ""}`}
                onClick={() => setActive(n.id)}
              >
                <span className="sidebar-item__icon">{n.icon}</span>
                <span>{t(n.labelKey)}</span>
              </button>
            ))}
          </nav>

          <div className="modal__body settings-modal__content">
            {active === "general" && <GeneralSettings />}
            {active === "models" && <ModelsSettings />}
            {active === "updates" && <UpdatesSettings />}
            {active === "data" && (
              <DataSettings
                onImportAgent={onImportAgent}
                onImportPreset={onImportPreset}
              />
            )}
            {active === "about" && <AboutSettings />}
          </div>
        </div>
      </div>
    </div>
  );
}
