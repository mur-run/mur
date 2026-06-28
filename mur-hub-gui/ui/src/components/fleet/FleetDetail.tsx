import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useT } from "../../i18n";
import type { TranslationKey } from "../../i18n/types";
import type { FleetDetail as Detail, JobRow } from "./types";

interface Props {
  detail: Detail;
  jobs: JobRow[];
  onRefresh: () => void;
  onDelete: () => void;
}

function showToast(msg: string, durationMs = 2500) {
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = msg;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), durationMs);
}

function statusBadge(d: Detail): string {
  if (d.stopped) return "⏸ stopped";
  return "● idle";
}

function jobStatusClass(status: JobRow["status"]): string {
  return `fleet-job__status fleet-job__status--${status}`;
}

export function FleetDetail({ detail, jobs, onRefresh, onDelete }: Props) {
  const { t } = useT();
  const [busy, setBusy] = useState<string | null>(null);
  const [sendInput, setSendInput] = useState("");
  const [addInput, setAddInput] = useState("");
  const [showAll, setShowAll] = useState(false);

  async function call(cmd: string, args: Record<string, unknown>) {
    setBusy(cmd);
    try {
      await invoke(cmd, args);
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  async function handleRun() {
    showToast(t("fleet.runStarted"));
    await call("fleet_run", { name: detail.name });
  }

  async function handleSend() {
    const text = sendInput.trim();
    if (!text) return;
    setBusy("fleet_send");
    try {
      const jobId = await invoke<string>("fleet_send", {
        name: detail.name,
        text,
      });
      setSendInput("");
      showToast(`Job queued: ${jobId.slice(0, 8)}`);
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  async function handleAddMember() {
    const agent = addInput.trim();
    if (!agent) return;
    await call("fleet_add_member", { name: detail.name, agent });
    setAddInput("");
  }

  async function handleExport() {
    setBusy("fleet_export");
    try {
      const path = await invoke<string>("fleet_export", { name: detail.name });
      showToast(t("fleet.exported").replace("{path}", path), 4000);
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  async function handleImport() {
    const selected = await open({ filters: [{ name: "Fleet", extensions: ["fleet"] }] });
    if (!selected) return;
    const filePath = typeof selected === "string" ? selected : selected[0];
    if (!filePath) return;
    setBusy("fleet_import");
    try {
      const name = await invoke<string>("fleet_import", { path: filePath });
      showToast(t("fleet.imported").replace("{name}", name));
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  async function handleDelete() {
    const msg = t("fleet.confirmDelete").replace("{name}", detail.display_name);
    if (!window.confirm(msg)) return;
    setBusy("fleet_delete");
    try {
      await invoke("fleet_delete", { name: detail.name });
      onDelete();
    } catch (err) {
      showToast(String(err), 4000);
      setBusy(null);
    }
  }

  function handleShowAll() {
    setShowAll((prev) => !prev);
  }

  const displayedJobs = showAll ? jobs : jobs.slice(0, 10);

  return (
    <div className="fleet-detail">
      <header className="fleet-detail__header">
        <div className="fleet-detail__title">
          <h2>{detail.display_name}</h2>
          <span className="fleet-detail__status">{statusBadge(detail)}</span>
        </div>
        <p className="fleet-detail__goal">{detail.goal}</p>
        <div className="fleet-detail__meta">
          <span>
            {t("fleet.router")}: {detail.router}
          </span>
        </div>
        <div className="fleet-detail__toolbar">
          <button
            className="toolbar-btn toolbar-btn--primary"
            onClick={handleRun}
            disabled={busy !== null}
          >
            {t("fleet.run")}
          </button>
          {detail.stopped ? (
            <button
              className="toolbar-btn"
              onClick={() => call("fleet_start", { name: detail.name })}
              disabled={busy !== null}
            >
              {t("fleet.start")}
            </button>
          ) : (
            <button
              className="toolbar-btn"
              onClick={() => call("fleet_stop", { name: detail.name })}
              disabled={busy !== null}
            >
              {t("fleet.stop")}
            </button>
          )}
          <button
            className="toolbar-btn"
            onClick={handleExport}
            disabled={busy !== null}
          >
            {t("fleet.export")}
          </button>
          <button
            className="toolbar-btn"
            onClick={handleImport}
            disabled={busy !== null}
          >
            {t("fleet.import")}
          </button>
          <button
            className="toolbar-btn toolbar-btn--danger"
            onClick={handleDelete}
            disabled={busy !== null}
          >
            {t("fleet.delete")}
          </button>
        </div>
      </header>

      {/* Members */}
      <section className="fleet-detail__section">
        <h3>{t("fleet.members")}</h3>
        <ul className="fleet-members">
          {detail.members.map((m) => (
            <li key={m} className="fleet-members__row">
              <span>{m}</span>
              <button
                className="fleet-members__remove"
                onClick={() =>
                  call("fleet_remove_member", { name: detail.name, agent: m })
                }
                disabled={busy !== null}
              >
                {t("fleet.removeMember")}
              </button>
            </li>
          ))}
        </ul>
        <div className="fleet-members__add">
          <input
            value={addInput}
            onChange={(e) => setAddInput(e.target.value)}
            placeholder={t("fleet.addMember")}
            onKeyDown={(e) => e.key === "Enter" && handleAddMember()}
          />
          <button
            onClick={handleAddMember}
            disabled={busy !== null || !addInput.trim()}
          >
            +
          </button>
        </div>
      </section>

      {/* Send Job */}
      <section className="fleet-detail__section">
        <div className="fleet-send">
          <input
            value={sendInput}
            onChange={(e) => setSendInput(e.target.value)}
            placeholder={t("fleet.sendPlaceholder")}
            onKeyDown={(e) => e.key === "Enter" && handleSend()}
          />
          <button
            className="toolbar-btn toolbar-btn--primary"
            onClick={handleSend}
            disabled={busy !== null || !sendInput.trim()}
          >
            {t("fleet.send")}
          </button>
        </div>
      </section>

      {/* Jobs */}
      <section className="fleet-detail__section">
        <div className="fleet-jobs__header">
          <h3>{t("fleet.jobs")}</h3>
          {jobs.length > 10 && (
            <button className="fleet-jobs__show-all" onClick={handleShowAll}>
              {t("fleet.showAll")} ({jobs.length})
            </button>
          )}
        </div>
        <ul className="fleet-jobs">
          {displayedJobs.map((job) => (
            <li key={job.id} className="fleet-job">
              <span className={jobStatusClass(job.status)}>
                {t(`fleet.status.${job.status}` as TranslationKey)}
              </span>
              <span className="fleet-job__text">{job.text}</span>
              <span className="fleet-job__time">{job.created_at}</span>
              {job.error && (
                <span className="fleet-job__error">{job.error}</span>
              )}
            </li>
          ))}
        </ul>
      </section>
    </div>
  );
}
