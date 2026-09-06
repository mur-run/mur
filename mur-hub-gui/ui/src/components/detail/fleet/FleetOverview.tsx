import type { AgentEntry } from "../../../types";
import type { FleetDetail as Detail, JobRow } from "../../fleet/types";
import type { FleetTabId } from "../../shell/detailTabs";
import { useT } from "../../../i18n";
import type { TranslationKey } from "../../../i18n/types";
import { avatarPreset, familyOf } from "../../../utils";
import { PetFace } from "../../PetFace";

export interface FleetOverviewProps {
  detail: Detail;
  jobs: JobRow[];
  agentMap: Map<string, AgentEntry>;
  onGoTo: (tab: FleetTabId) => void;
}

const DASH = "—";
const JOB_PREVIEW = 3;

/** `last_run` is an ISO timestamp from the loop record; show it in the
 *  viewer's locale when it parses, else as given. */
function lastRunLabel(raw: string | null | undefined, never: string): string {
  if (!raw) return never;
  const ms = Date.parse(raw);
  return Number.isNaN(ms) ? raw : new Date(ms).toLocaleString();
}

/** Overview (spec §4.4): goal, the loop limits from `loop_cfg`, members, the
 *  first jobs, and the loop summary. Iterations used and budget spent have no
 *  Hub source yet and are not shown as numbers. */
export function FleetOverview({ detail, jobs, agentMap, onGoTo }: FleetOverviewProps) {
  const { t } = useT();
  const loop = detail.loop_cfg;
  const members = [detail.router, ...detail.members.filter((m) => m !== detail.router)];
  return (
    <>
      <div className="detail-card">
        <div className="detail-card__eyebrow">{t("fleet.goal")}</div>
        <p className="fleet-goal">{detail.goal}</p>
      </div>
      <div className="detail-card detail-stats">
        <div>
          <b>{lastRunLabel(loop?.last_run, t("fleet.never"))}</b>
          <span>{t("fleet.settings.lastRun")}</span>
        </div>
        <div>
          <b>{loop ? loop.max_iterations : DASH}</b>
          <span>{t("fleet.settings.maxIterations")}</span>
        </div>
        <div>
          <b>{loop ? `$${loop.budget_usd}` : DASH}</b>
          <span>{t("fleet.settings.budget")}</span>
        </div>
        <div>
          <b>{loop?.done_when || (loop ? t("fleet.settings.donePolicyRouter") : DASH)}</b>
          <span>{t("fleet.settings.doneWhen")}</span>
        </div>
      </div>
      <div className="detail-two">
        <div className="detail-card">
          <div className="detail-card__eyebrow">{t("fleet.members")}</div>
          <div className="fleet-overview__members">
            {members.map((m) => {
              const agent = agentMap.get(m) ?? agentMap.get(m.toLowerCase());
              return (
                <span key={m} className="fleet-overview__member">
                  {agent ? (
                    <PetFace presetId={avatarPreset(agent)} family={familyOf(avatarPreset(agent))} expression="idle" size={22} animate={false} />
                  ) : (
                    <span className="fleet-overview__initial">{m.charAt(0).toUpperCase()}</span>
                  )}
                  {agent?.display_name ?? m}
                  {m === detail.router && <i>{t("fleetInspector.router")}</i>}
                </span>
              );
            })}
            <button type="button" className="fleet-overview__add" onClick={() => onGoTo("members")}>
              {t("fleet.addMember")}
            </button>
          </div>
        </div>
        <div className="detail-card">
          <div className="detail-card__eyebrow">{t("fleet.jobs")}</div>
          {jobs.length === 0 && <p className="overview-now__sub">{t("fleet.noJobs")}</p>}
          {jobs.slice(0, JOB_PREVIEW).map((job) => (
            <div key={job.id} className="detail-kv fleet-overview__job">
              <span>{job.created_at.slice(0, 10)}</span>
              <span>{job.text}</span>
              <span className={`fleet-job__status fleet-job__status--${job.status}`}>
                {t(`fleet.status.${job.status}` as TranslationKey)}
              </span>
            </div>
          ))}
          {jobs.length > JOB_PREVIEW && (
            <button type="button" className="btn btn--link" onClick={() => onGoTo("jobs")}>
              {t("fleet.showAll")}
            </button>
          )}
        </div>
      </div>
      <div className="detail-card">
        <div className="detail-card__eyebrow">{t("fleetInspector.loop")}</div>
        {loop ? (
          <>
            <div className="detail-kv">
              <span>{t("fleet.settings.trigger")}</span>
              <span className="mono">{loop.trigger || t("fleet.settings.triggerManual")}</span>
              <button type="button" onClick={() => onGoTo("settings")}>{t("fleet.tab.settings")}</button>
            </div>
            <div className="detail-kv">
              <span>{t("fleet.settings.deadline")}</span>
              <span>{loop.deadline || DASH}</span>
              <button type="button" onClick={() => onGoTo("settings")}>{t("fleet.tab.settings")}</button>
            </div>
          </>
        ) : (
          <div className="detail-kv">
            <span>{t("fleet.settings.trigger")}</span>
            <span>{t("fleetInspector.noLoop")}</span>
            <button type="button" onClick={() => onGoTo("settings")}>{t("fleet.tab.settings")}</button>
          </div>
        )}
      </div>
    </>
  );
}
