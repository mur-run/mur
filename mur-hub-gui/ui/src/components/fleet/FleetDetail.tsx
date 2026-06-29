import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { confirm, open } from "@tauri-apps/plugin-dialog";
import { useT } from "../../i18n";
import type { TranslationKey } from "../../i18n/types";
import type { AgentEntry } from "../../types";
import { CATEGORY_COLORS, avatarPreset, familyOf } from "../../utils";
import { PetFace } from "../PetFace";
import type { FleetDetail as Detail, JobRow } from "./types";

interface Props {
  detail: Detail;
  jobs: JobRow[];
  agentMap: Map<string, AgentEntry>;
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

function statusPillClass(d: Detail): string {
  if (d.stopped) return "fleet-detail__status-pill fleet-detail__status-pill--stopped";
  return "fleet-detail__status-pill fleet-detail__status-pill--idle";
}

function statusLabel(d: Detail): string {
  if (d.stopped) return "⏸ stopped";
  return "● idle";
}

function jobStatusClass(status: JobRow["status"]): string {
  return `fleet-job__status fleet-job__status--${status}`;
}

export function FleetDetail({ detail, jobs, agentMap, onRefresh, onDelete }: Props) {
  const { t } = useT();
  const [busy, setBusy] = useState<string | null>(null);
  const [sendInput, setSendInput] = useState("");
  const [addInput, setAddInput] = useState("");
  const [showAll, setShowAll] = useState(false);
  const [allJobs, setAllJobs] = useState<JobRow[]>([]);

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
      const jobId = await invoke<string>("fleet_send", { name: detail.name, text });
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
    setBusy("fleet_add_member");
    try {
      await invoke("fleet_add_member", { name: detail.name, agent });
      setAddInput("");
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
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
    const ok = await confirm(msg, { title: t("fleet.delete"), kind: "warning" });
    if (!ok) return;
    setBusy("fleet_delete");
    try {
      await invoke("fleet_delete", { name: detail.name });
      onDelete();
    } catch (err) {
      showToast(String(err), 4000);
      setBusy(null);
    }
  }

  async function handleShowAll() {
    if (showAll) { setShowAll(false); setAllJobs([]); return; }
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

  return (
    <div className="fleet-detail">
      <div className="fleet-detail__header">
        <div className="fleet-detail__title-row">
          <h2 className="fleet-detail__title">{detail.display_name}</h2>
          <span className={statusPillClass(detail)}>{statusLabel(detail)}</span>
        </div>
        <p className="fleet-detail__goal">{detail.goal}</p>
        <p className="fleet-detail__router">{t("fleet.router")}: {detail.router}</p>
      </div>

      <div className="fleet-detail__actions">
        <button className="toolbar-btn toolbar-btn--primary" onClick={handleRun} disabled={busy !== null}>
          {t("fleet.run")}
        </button>
        {detail.stopped ? (
          <button className="toolbar-btn" onClick={() => call("fleet_start", { name: detail.name })} disabled={busy !== null}>
            {t("fleet.start")}
          </button>
        ) : (
          <button className="toolbar-btn" onClick={() => call("fleet_stop", { name: detail.name })} disabled={busy !== null}>
            {t("fleet.stop")}
          </button>
        )}
        <button className="toolbar-btn" onClick={handleExport} disabled={busy !== null}>{t("fleet.export")}</button>
        <button className="toolbar-btn" onClick={handleImport} disabled={busy !== null}>{t("fleet.import")}</button>
        <button className="toolbar-btn toolbar-btn--danger" onClick={handleDelete} disabled={busy !== null}>{t("fleet.delete")}</button>
      </div>

      <div className="fleet-section">
        <div className="fleet-section__label">{t("fleet.members")}</div>
        <div className="fleet-members">
          {detail.members.map((m) => {
            const agent = agentMap.get(m) ?? agentMap.get(m.toLowerCase());
            const color = agent ? (CATEGORY_COLORS[agent.category] ?? "#6B7280") : "#6B7280";
            return (
            <div key={m} className="fleet-member">
              <div className="fleet-member__avatar" style={agent ? {} : { background: color }}>
                {agent ? (
                  <PetFace presetId={avatarPreset(agent)} family={familyOf(avatarPreset(agent))} expression="idle" size={24} animate={false} />
                ) : (
                  <span style={{ fontSize: 12, color: "#fff", fontWeight: 600 }}>
                    {m.charAt(0).toUpperCase()}
                  </span>
                )}
              </div>
              <span className="fleet-member__name">{agent?.display_name ?? m}</span>
              <button
                className="fleet-member__remove"
                onClick={() => call("fleet_remove_member", { name: detail.name, agent: m })}
                disabled={busy !== null}
              >
                ✕
              </button>
            </div>
            );
          })}
        </div>
        {/* Add member: searchable combobox */}
        <div className="fleet-add-member">
          <div className="fleet-add-member__combo">
            <input
              value={addInput}
              onChange={(e) => { setAddInput(e.target.value); }}
              placeholder={t("fleet.addMember")}
              onKeyDown={(e) => {
                if (e.key === "Enter") handleAddMember();
                if (e.key === "Escape") setAddInput("");
              }}
              autoComplete="off"
            />
            {addInput.length > 0 && (() => {
              const lower = addInput.toLowerCase();
              const memberSet = new Set(detail.members.map(m => m.toLowerCase()));
              const suggestions = Array.from(agentMap.values()).filter(
                (a) => !memberSet.has(a.name.toLowerCase()) &&
                  (a.name.toLowerCase().includes(lower) || a.display_name.toLowerCase().includes(lower))
              );
              return suggestions.length > 0 ? (
                <ul className="fleet-add-member__suggestions">
                  {suggestions.map((a) => (
                    <li key={a.name} onMouseDown={() => { setAddInput(a.name); }}>
                      {a.display_name}
                    </li>
                  ))}
                </ul>
              ) : null;
            })()}
          </div>
          <button className="toolbar-btn" onClick={handleAddMember} disabled={busy !== null || !addInput.trim()}>+</button>
        </div>
      </div>

      <div className="fleet-section">
        <div className="fleet-section__label">{t("fleet.send")}</div>
        <div className="fleet-send">
          <input
            value={sendInput}
            onChange={(e) => setSendInput(e.target.value)}
            placeholder={t("fleet.sendPlaceholder")}
            onKeyDown={(e) => e.key === "Enter" && handleSend()}
          />
          <button className="toolbar-btn toolbar-btn--primary" onClick={handleSend} disabled={busy !== null || !sendInput.trim()}>
            →
          </button>
        </div>
      </div>

      <div className="fleet-section">
        <div className="fleet-section__label">{t("fleet.jobs")}</div>
        <div className="fleet-jobs">
          {displayedJobs.length === 0 && (
            <span style={{ fontSize: 12, color: "var(--text-tertiary)" }}>No jobs yet.</span>
          )}
          {displayedJobs.map((job) => (
            <div key={job.id} className="fleet-job">
              <span className={jobStatusClass(job.status)}>
                {t(`fleet.status.${job.status}` as TranslationKey)}
              </span>
              <span className="fleet-job__text">{job.text}</span>
              <span className="fleet-job__ts">{job.created_at.slice(0, 10)}</span>
            </div>
          ))}
        </div>
        <button className="fleet-jobs__more" onClick={handleShowAll} disabled={busy !== null}>
          {showAll ? "Show active only" : t("fleet.showAll")}
        </button>
      </div>
    </div>
  );
}
