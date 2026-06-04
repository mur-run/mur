import type { BehaviorPreset, WizardSnapshot } from "../../../types";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../../i18n";
import type { TranslationKey } from "../../../i18n/types";

const BEHAVIORS: { id: BehaviorPreset; labelKey: TranslationKey; descKey: TranslationKey }[] = [
  { id: "quiet",  labelKey: "wizard.behavior.quiet",  descKey: "wizard.behavior.quietDesc" },
  { id: "normal", labelKey: "wizard.behavior.normal", descKey: "wizard.behavior.normalDesc" },
  { id: "lively", labelKey: "wizard.behavior.lively", descKey: "wizard.behavior.livelyDesc" },
];

interface Props {
  snapshot: WizardSnapshot;
  onUpdate: (s: WizardSnapshot) => void;
}

export function Step4Behavior({ snapshot, onUpdate }: Props) {
  const { t } = useT();
  async function pick(id: BehaviorPreset) {
    const s: WizardSnapshot = await invoke("wizard_set_behavior", { behavior: id });
    onUpdate(s);
  }

  return (
    <div className="wz-step">
      <h2>{t("wizard.behavior.title")}</h2>
      <p className="wz-hint">{t("wizard.behavior.hint")}</p>

      <div className="wz-behavior-list">
        {BEHAVIORS.map(({ id, labelKey, descKey }) => (
          <button
            key={id}
            className={`wz-behavior-card${snapshot.behavior_preset === id ? " selected" : ""}`}
            onClick={() => pick(id)}
          >
            <span className="wz-behavior-label">{t(labelKey)}</span>
            <span className="wz-behavior-desc">{t(descKey)}</span>
          </button>
        ))}
      </div>
    </div>
  );
}
