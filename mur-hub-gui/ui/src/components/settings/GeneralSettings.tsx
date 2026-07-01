import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import { applyTheme, getStoredTheme, type ThemeChoice } from "../../theme";

const THEMES: ThemeChoice[] = ["system", "light", "dark"];

function showToast(msg: string, durationMs = 2000) {
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = msg;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), durationMs);
}

export function GeneralSettings() {
  const { t, lang, setLang } = useT();
  const [theme, setTheme] = useState<ThemeChoice>(getStoredTheme);
  const [fleetAutorun, setFleetAutorun] = useState(false);

  useEffect(() => {
    invoke<boolean>("get_fleet_autorun").then(setFleetAutorun);
  }, []);

  async function handleFleetAutorunToggle(checked: boolean) {
    setFleetAutorun(checked);
    try {
      await invoke("set_fleet_autorun", { enabled: checked });
    } catch (err) {
      setFleetAutorun(!checked);
      showToast(String(err), 4000);
    }
  }

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

      <div className="settings-row">
        <label className="settings-row__label" htmlFor="settings-fleet-autorun">
          {t("settings.fleetAutorun.label")}
        </label>
        <input
          id="settings-fleet-autorun"
          type="checkbox"
          checked={fleetAutorun}
          onChange={(e) => handleFleetAutorunToggle(e.target.checked)}
        />
      </div>
      <p className="settings-row__hint">{t("settings.fleetAutorun.description")}</p>
    </section>
  );
}
