import type { Channel, ChannelActor } from "../../work/types";
import type { AgentEntry } from "../../types";
import type { TranslationKey } from "../../i18n/types";
import { stateBadge, actorName } from "../../work/format";
import { avatarPreset, familyOf } from "../../utils";
import { PetFace } from "../PetFace";
import { useT } from "../../i18n";

/** Small avatar for a run participant — the agent's PetFace, else a glyph. */
function ParticipantAvatar({
  actor,
  agents,
}: {
  actor: ChannelActor;
  agents: AgentEntry[];
}) {
  if (actor.kind === "agent") {
    const entry = agents.find((a) => a.name === actor.id);
    if (entry) {
      const preset = avatarPreset(entry);
      return (
        <span className="work-trace__pavatar">
          <PetFace
            presetId={preset}
            family={familyOf(preset)}
            expression="idle"
            size={24}
            animate={false}
          />
        </span>
      );
    }
    return <span className="work-trace__pavatar work-trace__pavatar--glyph">🤖</span>;
  }
  if (actor.kind === "human")
    return <span className="work-trace__pavatar work-trace__pavatar--glyph">🧑</span>;
  return <span className="work-trace__pavatar work-trace__pavatar--glyph">⚙️</span>;
}

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
  agents: AgentEntry[];
}

export function WorkTrace({ channel, displayNames, agents }: Props) {
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
              <ParticipantAvatar actor={p.actor} agents={agents} />
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
