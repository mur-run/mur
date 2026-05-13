import { useState } from "react";
import type { WizardSnapshot } from "../../../types";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

interface Props {
  snapshot: WizardSnapshot;
  onUpdate: (s: WizardSnapshot) => void;
  onSkip: () => void;
}

export function Step5Photo({ snapshot, onUpdate, onSkip }: Props) {
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
      <h2>Upload source photo</h2>
      <p className="wz-hint">
        The <strong>Family Photo</strong> preset uses your photo as a reference to generate a
        personalized cartoon companion. Your photo stays local — it is never uploaded.
      </p>

      {snapshot.source_photo ? (
        <div className="wz-photo-selected">
          <span>✅ {snapshot.source_photo.split("/").pop()}</span>
          <button className="wz-btn secondary" onClick={pickPhoto}>
            Change photo
          </button>
        </div>
      ) : (
        <button className="wz-btn primary" onClick={pickPhoto}>
          📁 Choose photo…
        </button>
      )}

      {error && <p className="wz-error">{error}</p>}

      <div className="wz-actions">
        <button className="wz-btn ghost" onClick={onSkip}>
          Skip (use placeholder)
        </button>
      </div>
    </div>
  );
}
