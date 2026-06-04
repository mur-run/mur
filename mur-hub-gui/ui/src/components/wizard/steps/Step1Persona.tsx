import type { WizardPersona, WizardSnapshot } from "../../../types";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../../i18n";
import type { TranslationKey } from "../../../i18n/types";

const PERSONAS: { id: WizardPersona; labelKey: TranslationKey; icon: string }[] = [
  { id: "research",   labelKey: "wizard.persona.research",   icon: "🔍" },
  { id: "automation", labelKey: "wizard.persona.automation", icon: "⚙️" },
  { id: "monitor",    labelKey: "wizard.persona.monitor",    icon: "📡" },
  { id: "notify",     labelKey: "wizard.persona.notify",     icon: "🔔" },
  { id: "commerce",   labelKey: "wizard.persona.commerce",   icon: "🛒" },
  { id: "custom",     labelKey: "wizard.persona.custom",     icon: "✨" },
];

interface Props {
  snapshot: WizardSnapshot;
  onUpdate: (s: WizardSnapshot) => void;
}

export function Step1Persona({ snapshot, onUpdate }: Props) {
  const { t } = useT();
  async function pick(id: WizardPersona) {
    const next: WizardSnapshot = await invoke("wizard_set_persona", { persona: id });
    onUpdate(next);
  }

  return (
    <div className="wz-step">
      <h2>{t("wizard.persona.title")}</h2>
      <p className="wz-hint">{t("wizard.persona.hint")}</p>
      <div className="wz-persona-grid">
        {PERSONAS.map(({ id, labelKey, icon }) => (
          <button
            key={id}
            className={`wz-persona-card${snapshot.persona === id ? " selected" : ""}`}
            onClick={() => pick(id)}
          >
            <span className="wz-persona-icon">{icon}</span>
            <span className="wz-persona-label">{t(labelKey)}</span>
          </button>
        ))}
      </div>
    </div>
  );
}
