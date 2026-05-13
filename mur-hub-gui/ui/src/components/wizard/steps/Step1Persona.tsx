import type { WizardPersona, WizardSnapshot } from "../../../types";
import { invoke } from "@tauri-apps/api/core";

const PERSONAS: { id: WizardPersona; label: string; icon: string }[] = [
  { id: "research",   label: "Research",   icon: "🔍" },
  { id: "automation", label: "Automation", icon: "⚙️" },
  { id: "monitor",    label: "Monitor",    icon: "📡" },
  { id: "notify",     label: "Notify",     icon: "🔔" },
  { id: "commerce",   label: "Commerce",   icon: "🛒" },
  { id: "custom",     label: "Custom",     icon: "✨" },
];

interface Props {
  snapshot: WizardSnapshot;
  onUpdate: (s: WizardSnapshot) => void;
}

export function Step1Persona({ snapshot, onUpdate }: Props) {
  async function pick(id: WizardPersona) {
    const next: WizardSnapshot = await invoke("wizard_set_persona", { persona: id });
    onUpdate(next);
  }

  return (
    <div className="wz-step">
      <h2>What does this agent do?</h2>
      <p className="wz-hint">Choose a persona category to get started.</p>
      <div className="wz-persona-grid">
        {PERSONAS.map(({ id, label, icon }) => (
          <button
            key={id}
            className={`wz-persona-card${snapshot.persona === id ? " selected" : ""}`}
            onClick={() => pick(id)}
          >
            <span className="wz-persona-icon">{icon}</span>
            <span className="wz-persona-label">{label}</span>
          </button>
        ))}
      </div>
    </div>
  );
}
