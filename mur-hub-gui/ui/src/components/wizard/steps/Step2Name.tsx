import { useState } from "react";
import type { WizardSnapshot } from "../../../types";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../../i18n";

interface Props {
  snapshot: WizardSnapshot;
  onUpdate: (s: WizardSnapshot) => void;
}

export function Step2Name({ snapshot, onUpdate }: Props) {
  const { t } = useT();
  const [name, setName] = useState(snapshot.name ?? "");
  const [desc, setDesc] = useState(snapshot.description ?? "");
  const [error, setError] = useState<string | null>(null);

  async function next() {
    setError(null);
    try {
      const s: WizardSnapshot = await invoke("wizard_set_name", { name, description: desc });
      onUpdate(s);
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div className="wz-step">
      <h2>{t("wizard.name.title")}</h2>
      <p className="wz-hint">{t("wizard.name.hint")}</p>

      <label className="wz-label">
        {t("wizard.name.label")}
        <input
          className="wz-input"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder={t("wizard.name.placeholder")}
          autoFocus
          onKeyDown={(e) => e.key === "Enter" && next()}
        />
      </label>

      <label className="wz-label">
        {t("wizard.name.descLabel")} <span className="wz-optional">{t("wizard.name.optional")}</span>
        <textarea
          className="wz-textarea"
          value={desc}
          onChange={(e) => setDesc(e.target.value)}
          placeholder={t("wizard.name.descPlaceholder")}
          rows={3}
        />
      </label>

      {error && <p className="wz-error">{error}</p>}

      <div className="wz-actions">
        <button className="btn btn--primary" onClick={next} disabled={!name.trim()}>
          {t("wizard.next")} →
        </button>
      </div>
    </div>
  );
}
