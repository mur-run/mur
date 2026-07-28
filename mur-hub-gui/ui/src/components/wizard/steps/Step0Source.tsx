import { useT } from "../../../i18n";
import type { TranslationKey } from "../../../i18n/types";
import type { AgentSource } from "../specFlow";

interface Props {
  onSelect: (source: AgentSource) => void;
}

const SOURCES: {
  id: AgentSource;
  labelKey: TranslationKey;
  icon: string;
  hintKey: TranslationKey;
  getsKey: TranslationKey;
}[] = [
  {
    id: "template",
    labelKey: "wizard.source.template",
    icon: "⚙️",
    hintKey: "wizard.source.template.hint",
    getsKey: "wizard.source.template.gets",
  },
  {
    id: "official",
    labelKey: "wizard.source.official",
    icon: "🏅",
    hintKey: "wizard.source.official.hint",
    getsKey: "wizard.source.official.gets",
  },
  {
    id: "import",
    labelKey: "wizard.source.import",
    icon: "📦",
    hintKey: "wizard.source.import.hint",
    getsKey: "wizard.source.import.gets",
  },
];

/**
 * Step 0 — where does this agent come from? Deliberately NOT "what kind of
 * agent": every agent can be given a pet appearance (the wizard's last step),
 * so kind was never a fork in the first place.
 */
export function Step0Source({ onSelect }: Props) {
  const { t } = useT();

  return (
    <div className="wz-step">
      <h2>{t("wizard.source.title")}</h2>
      <p className="wz-hint">{t("wizard.source.subtitle")}</p>
      <div className="wz-persona-grid">
        {SOURCES.map(({ id, labelKey, icon, hintKey, getsKey }) => (
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
