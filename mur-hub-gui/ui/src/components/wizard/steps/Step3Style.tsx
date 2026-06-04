import type { WizardSnapshot, PresetSummary } from "../../../types";
import { BUILTIN_PRESETS } from "../../../types";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../../i18n";

const FAMILY_EMOJI: Record<string, string> = {
  chibi:    "🐣",
  pixel:    "🕹️",
  live2d:   "🎭",
  polaroid: "📸",
};

interface Props {
  snapshot: WizardSnapshot;
  onUpdate: (s: WizardSnapshot) => void;
}

export function Step3Style({ snapshot, onUpdate }: Props) {
  const { t } = useT();
  async function pick(id: string) {
    const s: WizardSnapshot = await invoke("wizard_set_preset", { presetId: id });
    onUpdate(s);
  }

  return (
    <div className="wz-step">
      <h2>{t("wizard.style.title")}</h2>
      <p className="wz-hint">{t("wizard.style.hint")}</p>

      <div className="wz-preset-grid">
        {BUILTIN_PRESETS.map((p: PresetSummary) => (
          <button
            key={p.id}
            className={`wz-preset-card${snapshot.style_preset_id === p.id ? " selected" : ""}`}
            onClick={() => pick(p.id)}
          >
            <span className="wz-preset-family-icon">
              {FAMILY_EMOJI[p.family] ?? "🎨"}
            </span>
            <span className="wz-preset-name">{p.display_name}</span>
            <span className="wz-preset-desc">{p.description}</span>
            <span className="wz-preset-family-tag">{p.family}</span>
          </button>
        ))}
      </div>
    </div>
  );
}
