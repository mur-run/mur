import type { Channel } from "../../work/types";
import type { TranslationKey } from "../../i18n/types";
import { stateBadge, actorName } from "../../work/format";
import { useT } from "../../i18n";

const STATE_LABEL_KEYS: Partial<Record<string, TranslationKey>> = {
  submitted: "work.state.submitted",
  working: "work.state.working",
  "input-required": "work.state.input-required",
  completed: "work.state.completed",
  failed: "work.state.failed",
  canceled: "work.state.canceled",
  rejected: "work.state.rejected",
};

interface Props {
  channel: Channel | null;
  displayNames: Record<string, string>;
}

export function WorkTrace({ channel, displayNames }: Props) {
  const { t } = useT();
  if (!channel) return null;

  const badge = stateBadge(channel.state);

  return (
    <div className="work-trace">
      <div className="work-trace__section">
        <div className="work-trace__label">{t("work.state")}</div>
        <span className={`work-badge work-badge--${badge}`}>
          {(() => {
            const k = STATE_LABEL_KEYS[channel.state];
            return k ? t(k) : channel.state;
          })()}
        </span>
      </div>

      <div className="work-trace__section">
        <div className="work-trace__label">{t("work.goal")}</div>
        <div className="work-trace__goal">
          {channel.goal.statement || (
            <span className="work-trace__empty">{t("work.noGoal")}</span>
          )}
        </div>
        {channel.goal.acceptance_criteria.length > 0 && (
          <ul className="work-trace__ac">
            {channel.goal.acceptance_criteria.map((c, i) => (
              <li key={i}>{c}</li>
            ))}
          </ul>
        )}
      </div>

      <div className="work-trace__section">
        <div className="work-trace__label">{t("work.participants")}</div>
        <ul className="work-trace__plist">
          {channel.participants.map((p, i) => (
            <li key={i} className={`work-trace__prole work-trace__prole--${p.role}`}>
              <span className="work-trace__pname">
                {actorName(p.actor, displayNames)}
              </span>
              <span className="work-trace__prole-badge">{p.role}</span>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
