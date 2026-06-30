import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";

export function UpdatesSettings() {
  const { t } = useT();
  const [skew, setSkew] = useState<{ cli: string; hub: string } | null>(null);
  const [installMsg, setInstallMsg] = useState<string | null>(null);

  useEffect(() => {
    invoke<{ cli: string; hub: string } | null>("cli_version_skew")
      .then(setSkew)
      .catch(() => {});
  }, []);

  async function install() {
    try {
      const path = await invoke<string>("install_cli_tools");
      setInstallMsg(t("settings.cli.installed", { path }));
    } catch (e) {
      setInstallMsg(t("settings.cli.installFailed", { error: String(e) }));
    }
  }

  return (
    <section className="settings-section">
      <h3 className="settings-section__title">{t("settings.nav.updates")}</h3>
      <div className="settings-row">
        <span className="settings-row__value">
          {skew ? t("dashboard.cliSkew", skew) : t("settings.cli.inSync")}
        </span>
      </div>
      <div className="settings-row">
        <button className="toolbar-btn" onClick={install}>
          {t("settings.cli.install")}
        </button>
      </div>
      {installMsg && (
        <div className="settings-row">
          <span className="settings-row__value">{installMsg}</span>
        </div>
      )}
    </section>
  );
}
