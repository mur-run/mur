import { useState } from "react";
import type { WizardSnapshot } from "../../../types";
import { invoke } from "@tauri-apps/api/core";

interface Props {
  snapshot: WizardSnapshot;
  onUpdate: (s: WizardSnapshot) => void;
}

export function Step2Name({ snapshot, onUpdate }: Props) {
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
      <h2>Name your agent</h2>
      <p className="wz-hint">Use lowercase letters and hyphens only (e.g. <code>my-researcher</code>).</p>

      <label className="wz-label">
        Agent name
        <input
          className="wz-input"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="my-agent"
          autoFocus
          onKeyDown={(e) => e.key === "Enter" && next()}
        />
      </label>

      <label className="wz-label">
        Short description <span className="wz-optional">(optional)</span>
        <textarea
          className="wz-textarea"
          value={desc}
          onChange={(e) => setDesc(e.target.value)}
          placeholder="This agent helps me…"
          rows={3}
        />
      </label>

      {error && <p className="wz-error">{error}</p>}

      <div className="wz-actions">
        <button className="wz-btn primary" onClick={next} disabled={!name.trim()}>
          Continue →
        </button>
      </div>
    </div>
  );
}
