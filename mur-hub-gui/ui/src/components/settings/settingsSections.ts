import type { TranslationKey } from "../../i18n/types";

/** The Settings page's sections (spec 3(d) §4), in nav order. */
export type SettingsSectionId = "general" | "models" | "updates" | "data" | "about";

export const SETTINGS_SECTIONS: { id: SettingsSectionId; labelKey: TranslationKey }[] = [
  { id: "general", labelKey: "settings.nav.general" },
  { id: "models", labelKey: "settings.nav.models" },
  { id: "updates", labelKey: "settings.nav.updates" },
  { id: "data", labelKey: "settings.nav.data" },
  { id: "about", labelKey: "settings.nav.about" },
];

export function isSettingsSection(id: string): id is SettingsSectionId {
  return SETTINGS_SECTIONS.some((s) => s.id === id);
}

export const LAST_SECTION_KEY = "mur.settings.lastSection";
