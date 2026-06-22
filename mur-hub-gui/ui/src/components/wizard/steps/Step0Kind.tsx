import { useT } from "../../../i18n";
import type { TranslationKey } from "../../../i18n/types";
import type { AgentKind } from "../specFlow";

interface Props {
  onSelect: (kind: AgentKind) => void;
}

const KIND_OPTIONS: {
  id: AgentKind;
  labelKey: TranslationKey;
  icon: string;
  hintKey: TranslationKey;
  getsKey: TranslationKey;
}[] = [
  {
    id: "companion",
    labelKey: "wizard.kind.companion",
    icon: "🐦",
    hintKey: "wizard.kind.companion.hint",
    getsKey: "wizard.kind.companion.gets",
  },
  {
    id: "specialist",
    labelKey: "wizard.kind.specialist",
    icon: "⚙️",
    hintKey: "wizard.kind.specialist.hint",
    getsKey: "wizard.kind.specialist.gets",
  },
  {
    id: "both",
    labelKey: "wizard.kind.both",
    icon: "✨",
    hintKey: "wizard.kind.both.hint",
    getsKey: "wizard.kind.both.gets",
  },
];

export function Step0Kind({ onSelect }: Props) {
  const { t } = useT();

  return (
    <div className="wz-step">
      <h2>{t("wizard.kind.title")}</h2>
      <div className="wz-persona-grid">
        {KIND_OPTIONS.map(({ id, labelKey, icon, hintKey, getsKey }) => (
          <button
            key={id}
            className="wz-persona-card"
            onClick={() => onSelect(id)}
          >
            <span className="wz-persona-icon">{icon}</span>
            <span className="wz-persona-label">{t(labelKey)}</span>
            <span className="wz-persona-hint">{t(hintKey)}</span>
            <span className="wz-persona-gets">{t(getsKey)}</span>
          </button>
        ))}
      </div>
    </div>
  );
}
