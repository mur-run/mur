import type { BehaviorPreset, WizardSnapshot } from "../../../types";
import { invoke } from "@tauri-apps/api/core";

const BEHAVIORS: { id: BehaviorPreset; label: string; desc: string }[] = [
  { id: "quiet",  label: "Quiet",  desc: "Stays still, no proactive messages" },
  { id: "normal", label: "Normal", desc: "Gentle wander, voice on, daily check-ins" },
  { id: "lively", label: "Lively", desc: "Frequent expressions, enthusiastic reactions" },
];

interface Props {
  snapshot: WizardSnapshot;
  onUpdate: (s: WizardSnapshot) => void;
}

export function Step4Behavior({ snapshot, onUpdate }: Props) {
  async function pick(id: BehaviorPreset) {
    const s: WizardSnapshot = await invoke("wizard_set_behavior", { behavior: id });
    onUpdate(s);
  }

  return (
    <div className="wz-step">
      <h2>Choose behavior</h2>
      <p className="wz-hint">How active should your companion pet be?</p>

      <div className="wz-behavior-list">
        {BEHAVIORS.map(({ id, label, desc }) => (
          <button
            key={id}
            className={`wz-behavior-card${snapshot.behavior_preset === id ? " selected" : ""}`}
            onClick={() => pick(id)}
          >
            <span className="wz-behavior-label">{label}</span>
            <span className="wz-behavior-desc">{desc}</span>
          </button>
        ))}
      </div>
    </div>
  );
}
