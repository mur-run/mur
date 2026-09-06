import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { confirm } from "@tauri-apps/plugin-dialog";
import type { FleetDetail as Detail, JobRow } from "../../fleet/types";
import { useT } from "../../../i18n";
import type { TranslationKey } from "../../../i18n/types";
import { showToast, useFleetCall } from "./fleetActions";

export interface FleetJobsProps {
  detail: Detail;
  jobs: JobRow[];
  onRefresh: () => void;
}

const TERMINAL: ReadonlyArray<JobRow["status"]> = ["done", "failed", "canceled"];

function jobStatusClass(status: JobRow["status"]): string {
  return `fleet-job__status fleet-job__status--${status}`;
}

/** Jobs tab (spec §4.4): the job list with the active/all toggle and cancel.
 *  Sending a job lives in the header's Run ▾ menu. */
export function FleetJobs({ detail, jobs, onRefresh }: FleetJobsProps) {
  const { t } = useT();
  const { busy, setBusy } = useFleetCall(onRefresh);
  const [showAll, setShowAll] = useState(false);
  const [allJobs, setAllJobs] = useState<JobRow[]>([]);

  async function handleCancelJob(job: { id: string; text: string }) {
    const msg = t("fleet.confirmCancelJob").replace("{job}", job.text.split("\n")[0].slice(0, 60));
    const ok = await confirm(msg, { title: t("fleet.cancelJob"), kind: "warning" });
    if (!ok) return;
    setBusy("fleet_cancel_job");
    try {
      await invoke("fleet_cancel_job", { name: detail.name, id: job.id });
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  async function handleShowAll() {
    if (showAll) {
      setShowAll(false);
      setAllJobs([]);
      return;
    }
    setBusy("fleet_jobs");
    try {
      const rows = await invoke<JobRow[]>("fleet_jobs", { name: detail.name, all: true });
      setAllJobs(rows);
      setShowAll(true);
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  const displayedJobs = showAll ? allJobs : jobs;
  const active = jobs.filter((j) => !TERMINAL.includes(j.status)).length;

  return (
    <section className="detail-section fleet-section--jobs" id="fleet-jobs">
      <div className="fleet-section__header">
        <h3 className="detail-section__title">{t("fleet.jobs")}</h3>
        {active > 0 && <span className="fleet-section__badge">{active}</span>}
      </div>
      <div className="fleet-jobs">
        {displayedJobs.length === 0 && (
          <span className="fleet-jobs__empty">{t("fleet.noJobs")}</span>
        )}
        {displayedJobs.map((job) => (
          <div key={job.id} className="fleet-job">
            <span className={jobStatusClass(job.status)}>
              {t(`fleet.status.${job.status}` as TranslationKey)}
            </span>
            <span className="fleet-job__text">{job.text}</span>
            <span className="fleet-job__ts">{job.created_at.slice(0, 10)}</span>
            {job.status === "queued" && (
              <button
                className="fleet-job__cancel"
                title={t("fleet.cancelJob")}
                onClick={() => handleCancelJob(job)}
                disabled={busy !== null}
              >
                ×
              </button>
            )}
          </div>
        ))}
      </div>
      <button className="fleet-jobs__more" onClick={handleShowAll} disabled={busy !== null}>
        {showAll ? t("fleet.showActive") : t("fleet.showAll")}
      </button>
    </section>
  );
}
