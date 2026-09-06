import { useEffect, useRef, useState } from "react";
import { useT } from "../../i18n";
import { Ico } from "../agents/GridCard";
import { readKey, writeKey } from "../shell/persist";
import { GeneralSettings } from "./GeneralSettings";
import { ModelsSettings } from "./ModelsSettings";
import { UpdatesSettings } from "./UpdatesSettings";
import { DataSettings } from "./DataSettings";
import { AboutSettings } from "./AboutSettings";
import { SETTINGS_GLYPHS } from "./settingsGlyphs";
import {
  LAST_SECTION_KEY, SETTINGS_SECTIONS, isSettingsSection, type SettingsSectionId,
} from "./settingsSections";

const DEFAULT_SECTION: SettingsSectionId = "general";

export interface SettingsPageProps {
  /** A deep link (the model wizard's Customize → "models"); consumed once. */
  requestedSection?: SettingsSectionId | null;
  onRequestHandled?: () => void;
  onImportAgent: () => void;
  onImportPreset: () => void;
}

/** Settings on the shell (spec 3(d)): section nav | section content. */
export function SettingsPage({ requestedSection, onRequestHandled, onImportAgent, onImportPreset }: SettingsPageProps) {
  const { t } = useT();
  const [section, setSection] = useState<SettingsSectionId>(() => {
    const last = readKey(LAST_SECTION_KEY);
    return last && isSettingsSection(last) ? last : DEFAULT_SECTION;
  });
  // The mount render must not write the default over a stored section
  // before a request has had its say; write from the first change on.
  const mounted = useRef(false);
  useEffect(() => {
    if (mounted.current) writeKey(LAST_SECTION_KEY, section);
    mounted.current = true;
  }, [section]);

  useEffect(() => {
    if (!requestedSection) return;
    setSection(requestedSection);
    onRequestHandled?.();
  }, [requestedSection, onRequestHandled]);

  return (
    <div className="settings-page">
      <nav className="settings-nav" aria-label={t("settings.title")}>
        {SETTINGS_SECTIONS.map((s) => {
          const on = s.id === section;
          return (
            <button
              key={s.id}
              type="button"
              className={`settings-nav__item${on ? " settings-nav__item--active" : ""}`}
              aria-current={on ? "page" : undefined}
              onClick={() => setSection(s.id)}
            >
              <span className="settings-nav__icon"><Ico>{SETTINGS_GLYPHS[s.id]}</Ico></span>
              <span className="settings-nav__label">{t(s.labelKey)}</span>
            </button>
          );
        })}
      </nav>
      <div className="settings-page__content">
        {section === "general" && <GeneralSettings />}
        {section === "models" && <ModelsSettings />}
        {section === "updates" && <UpdatesSettings />}
        {section === "data" && <DataSettings onImportAgent={onImportAgent} onImportPreset={onImportPreset} />}
        {section === "about" && <AboutSettings />}
      </div>
    </div>
  );
}
