import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { useT } from "../../i18n";

const DOCS_URL = "https://app.mur.run/docs/core";
const REPO_URL = "https://github.com/mur-run/mur";

export function AboutSettings() {
  const { t } = useT();
  const [version, setVersion] = useState<string>("");
  const [replayed, setReplayed] = useState(false);

  useEffect(() => {
    getVersion().then(setVersion).catch(() => {});
  }, []);

  return (
    <section className="settings-section">
      <h3 className="settings-section__title">{t("settings.nav.about")}</h3>
      <div className="settings-row">
        <span className="settings-row__label">{t("settings.hubVersion")}</span>
        <span className="settings-row__value">MUR Hub {version}</span>
      </div>
      <div className="settings-row">
        <button className="toolbar-btn" onClick={() => void openExternal(DOCS_URL).catch(() => {})}>
          {t("settings.about.docs")}
        </button>
        <button className="toolbar-btn" onClick={() => void openExternal(REPO_URL).catch(() => {})}>
          {t("settings.about.github")}
        </button>
      </div>
      <div className="settings-row">
        <button
          className="toolbar-btn"
          onClick={() => {
            invoke("replay_onboarding").catch(() => {});
            setReplayed(true);
          }}
        >
          {t("settings.about.replay")}
        </button>
      </div>
      {replayed && (
        <div className="settings-row">
          <span className="settings-row__value">{t("settings.about.replayDone")}</span>
        </div>
      )}
    </section>
  );
}
