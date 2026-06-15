import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../../../i18n";

/** Backend DTO shape from wizard_spec_catalog (snake_case from serde). */
interface RoleChoice {
  id: string;
  display_name: string;
  charter: string;
  risk: string;
  skill_topics: string[];
  category: string;
}

interface Props {
  /** Called when the user clicks Generate with the chosen role id. */
  onStart: (roleId: string, noLlm: boolean) => void;
}

export function SpecRole({ onStart }: Props) {
  const { t } = useT();
  const [roles, setRoles] = useState<RoleChoice[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<RoleChoice[]>("wizard_spec_catalog")
      .then((r) => {
        setRoles(r);
        if (r.length > 0) setSelected(r[0].id);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  function handleGenerate() {
    if (!selected) return;
    onStart(selected, false);
  }

  return (
    <div className="wz-step">
      <h2>{t("wizard.role.title")}</h2>
      <p className="wz-hint">{t("wizard.role.hint")}</p>

      {loading && <p className="wz-progress-text">{t("wizard.loading")}</p>}

      {error && <p className="wz-error">{error}</p>}

      {!loading && !error && (
        <div className="wz-role-list">
          {roles.map((role) => (
            <label key={role.id} className="wz-role-option">
              <input
                type="radio"
                name="spec-role"
                value={role.id}
                checked={selected === role.id}
                onChange={() => setSelected(role.id)}
              />
              <span className="wz-role-info">
                <span className="wz-role-name">{role.display_name}</span>
                <span className="wz-role-charter">{role.charter}</span>
                <span className="wz-role-meta">
                  {t("wizard.role.risk")}: {role.risk} ·{" "}
                  {t("wizard.role.category")}: {role.category}
                </span>
              </span>
            </label>
          ))}
        </div>
      )}

      {!loading && !error && (
        <div className="wz-role-actions">
          <button
            className="btn btn--primary"
            disabled={!selected}
            onClick={handleGenerate}
          >
            {t("wizard.role.generate")}
          </button>
        </div>
      )}
    </div>
  );
}
