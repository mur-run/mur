import { useState } from "react";
import { useT } from "../../i18n";
import { applyTheme, getStoredTheme, type ThemeChoice } from "../../theme";

const THEMES: ThemeChoice[] = ["system", "light", "dark"];

export function GeneralSettings() {
  const { t, lang, setLang } = useT();
  const [theme, setTheme] = useState<ThemeChoice>(getStoredTheme);

  return (
    <section className="settings-section">
      <h3 className="settings-section__title">{t("settings.nav.general")}</h3>

      <div className="settings-row">
        <label className="settings-row__label" htmlFor="settings-lang">
          {t("settings.language")}
        </label>
        <select
          id="settings-lang"
          className="input"
          value={lang}
          onChange={(e) => setLang(e.target.value as typeof lang)}
        >
          <option value="en">English</option>
          <option value="zh-TW">繁體中文</option>
        </select>
      </div>

      <div className="settings-row">
        <label className="settings-row__label" htmlFor="settings-theme">
          {t("settings.theme")}
        </label>
        <select
          id="settings-theme"
          className="input"
          value={theme}
          onChange={(e) => {
            const next = e.target.value as ThemeChoice;
            setTheme(next);
            applyTheme(next);
          }}
        >
          {THEMES.map((c) => (
            <option key={c} value={c}>
              {t(`settings.theme.${c}` as Parameters<typeof t>[0])}
            </option>
          ))}
        </select>
      </div>
    </section>
  );
}
