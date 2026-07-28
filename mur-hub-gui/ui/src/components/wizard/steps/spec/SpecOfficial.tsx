import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../../../i18n";

/** Backend DTO from `official_list` (snake_case from serde). */
interface CatalogItemView {
  id: string;
  tier: string;
  version: string;
  description: string;
  agent_name: string | null;
}

interface Props {
  /**
   * Called after a successful install with the agent name, when the item was a
   * single agent. Fleets install several agents at once and have no single one
   * to dress up, so they report null and the wizard just closes.
   */
  onInstalled: (agentName: string | null) => void;
}

/**
 * Official-catalog source: browse app.mur.run's curated agents/fleets and
 * install one. The install itself (license verification, signature checks) is
 * mur-core's `official::install`, shared with `mur official install`.
 */
export function SpecOfficial({ onInstalled }: Props) {
  const { t } = useT();
  const [items, setItems] = useState<CatalogItemView[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loggedIn, setLoggedIn] = useState(true);
  const [installing, setInstalling] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<boolean>("official_logged_in").then(setLoggedIn).catch(() => {});
    invoke<CatalogItemView[]>("official_list")
      .then((list) => {
        setItems(list);
        if (list.length > 0) setSelected(list[0].id);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  async function install() {
    if (!selected) return;
    setInstalling(true);
    setError(null);
    try {
      const name = await invoke<string | null>("official_install", { id: selected });
      onInstalled(name);
    } catch (e) {
      setError(String(e));
    } finally {
      setInstalling(false);
    }
  }

  return (
    <div className="wz-step">
      <h2>{t("wizard.official.title")}</h2>
      <p className="wz-hint">{t("wizard.official.hint")}</p>

      {loading && <p className="wz-progress-text">{t("wizard.loading")}</p>}
      {error && <p className="wz-error">{error}</p>}
      {!loading && !error && items.length === 0 && (
        <p className="wz-hint">{t("wizard.official.empty")}</p>
      )}
      {!loading && !loggedIn && (
        <p className="wz-hint">{t("wizard.official.loginRequired")}</p>
      )}

      {items.length > 0 && (
        <div className="wz-role-list">
          {items.map((item) => (
            <label key={item.id} className="wz-role-option">
              <input
                type="radio"
                name="official-item"
                value={item.id}
                checked={selected === item.id}
                onChange={() => setSelected(item.id)}
              />
              <span className="wz-role-info">
                <span className="wz-role-name">{item.agent_name ?? item.id}</span>
                <span className="wz-role-charter">{item.description}</span>
                <span className="wz-role-meta">
                  {t("wizard.official.tier")}: {item.tier} · v{item.version}
                </span>
              </span>
            </label>
          ))}
        </div>
      )}

      {items.length > 0 && (
        <div className="wz-role-actions">
          <button
            className="btn btn--primary"
            disabled={!selected || installing || !loggedIn}
            onClick={install}
          >
            {installing ? t("wizard.official.installing") : t("wizard.official.install")}
          </button>
        </div>
      )}
    </div>
  );
}
