import { useState } from "react";
import type { WizardSnapshot } from "../../../types";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useT } from "../../../i18n";

interface Props {
  snapshot: WizardSnapshot;
  onUpdate: (s: WizardSnapshot) => void;
  onSkip: () => void;
}

export function Step5Photo({ snapshot, onUpdate, onSkip }: Props) {
  const { t } = useT();
  const [error, setError] = useState<string | null>(null);

  async function pickPhoto() {
    setError(null);
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: "Images", extensions: ["jpg", "jpeg", "png", "webp", "heic"] }],
      });
      if (!selected) return;
      const path = typeof selected === "string" ? selected : selected[0];
      const s: WizardSnapshot = await invoke("wizard_set_photo", { path });
      onUpdate(s);
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div className="wz-step">
      <h2>{t("wizard.photo.title")}</h2>
      <p className="wz-hint">{t("wizard.photo.hint")}</p>

      {snapshot.source_photo ? (
        <div className="wz-photo-selected">
          <span>✅ {snapshot.source_photo.split("/").pop()}</span>
          <button className="btn btn--secondary" onClick={pickPhoto}>
            {t("wizard.photo.change")}
          </button>
        </div>
      ) : (
        <button className="btn btn--primary" onClick={pickPhoto}>
          {t("wizard.photo.choose")}
        </button>
      )}

      {error && <p className="wz-error">{error}</p>}

      <div className="wz-actions">
        <button className="btn btn--secondary" onClick={onSkip}>
          {t("wizard.photo.skip")}
        </button>
      </div>
    </div>
  );
}
