import type { AgentEntry, AgentRuntimeStatus } from "../../types";
import type { ChannelSummary } from "../../work/types";
import { relativeTime, stateBadge } from "../../work/format";
import { isActivityChannel, isRunningChannel } from "./useChannels";
import { PetFace } from "../PetFace";
import { avatarPreset, familyOf } from "../../utils";
import { useT } from "../../i18n";

interface Props {
  agents: AgentEntry[];
  runtimeStatuses: AgentRuntimeStatus[];
  channels: ChannelSummary[];
  nowMs: number;
  /** Jump to the chats surface for a given channel/agent. */
  onOpen: () => void;
}

/**
 * "Now running" — live runtime agents plus active multi-agent runs (fleet
 * loops / workflows). Both are derived from the same data WorkView used:
 * runtime statuses (AgentContext) and `channel_list` activity channels.
 * Returns null when nothing is running so HomePage can show its empty state.
 */
export function NowRunning({
  agents,
  runtimeStatuses,
  channels,
  nowMs,
  onOpen,
}: Props) {
  const { t } = useT();

  const runningAgents = runtimeStatuses.filter(
    (s) => s.state.state === "running",
  );
  const runningChannels = channels
    .filter(isActivityChannel)
    .filter(isRunningChannel);

  if (runningAgents.length === 0 && runningChannels.length === 0) return null;

  const entryFor = (name: string) => agents.find((a) => a.name === name);

  return (
    <section className="home-section home-now-running">
      <h2 className="home-section__title">{t("home.nowRunning")}</h2>

      {runningAgents.length > 0 && (
        <div className="home-run-agents">
          {runningAgents.map((s) => {
            const entry = entryFor(s.name);
            const preset = entry ? avatarPreset(entry) : "starling";
            return (
              <div key={s.name} className="home-run-agent">
                <span className="home-run-agent__avatar">
                  <PetFace
                    presetId={preset}
                    family={familyOf(preset)}
                    expression="idle"
                    size={26}
                    animate={false}
                  />
                </span>
                <span className="home-run-agent__name">
                  {entry?.display_name ?? s.name}
                </span>
                <span className="home-run-agent__dot" aria-hidden="true" />
              </div>
            );
          })}
        </div>
      )}

      {runningChannels.length > 0 && (
        <ul className="home-run-list">
          {runningChannels.map((ch) => (
            <li key={ch.id}>
              <button className="home-run-row" onClick={onOpen}>
                <span className="home-run-row__title">
                  {ch.title || ch.id.slice(0, 8)}
                </span>
                <span className={`work-badge work-badge--${stateBadge(ch.state)}`}>
                  {ch.state}
                </span>
                <span className="home-run-row__meta">
                  {relativeTime(ch.updated_at, nowMs)}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
