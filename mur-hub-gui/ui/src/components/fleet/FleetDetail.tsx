import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { confirm, open, save } from "@tauri-apps/plugin-dialog";
import { useT } from "../../i18n";
import type { TranslationKey } from "../../i18n/types";
import type { AgentEntry } from "../../types";
import { CATEGORY_COLORS, avatarPreset, familyOf } from "../../utils";
import { PetFace } from "../PetFace";
import type { FleetDetail as Detail, JobRow, LabelView } from "./types";
import { DURATION_RE } from "./fleetCreateForm";
import { makePrimary, toggleAssignment } from "./fleetLabels";
import {
  parseTrigger,
  buildTrigger,
  settingsAreValid,
  modeBadgeLabel,
  loopDeadlineIsValid,
  type TriggerKind,
} from "./fleetSettingsForm";

interface Props {
  detail: Detail;
  jobs: JobRow[];
  agentMap: Map<string, AgentEntry>;
  /** The whole registry, in registry order — the chips offered here. */
  labels: LabelView[];
  /** This fleet's assigned label ids, primary first. */
  fleetLabels: string[];
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

export function FleetDetail({ detail, jobs, agentMap, labels, fleetLabels, onRefresh, onDelete }: Props) {
  const { t } = useT();
  const [busy, setBusy] = useState<string | null>(null);
  const [sendInput, setSendInput] = useState("");
  const [addInput, setAddInput] = useState("");
  const [showAll, setShowAll] = useState(false);
  const [allJobs, setAllJobs] = useState<JobRow[]>([]);

  const initialTrigger = parseTrigger(detail.loop_cfg);
  const [trigKind, setTrigKind] = useState<TriggerKind>(initialTrigger.kind);
  const [trigValue, setTrigValue] = useState(initialTrigger.value);
  const [maxIter, setMaxIter] = useState(
    detail.loop_cfg?.max_iterations ? String(detail.loop_cfg.max_iterations) : ""
  );
  const [deadline, setDeadlineValue] = useState(detail.loop_cfg?.deadline ?? "");
  const [budget, setBudget] = useState(
    detail.loop_cfg?.budget_usd ? String(detail.loop_cfg.budget_usd) : ""
  );
  const [doneWhen, setDoneWhen] = useState(detail.loop_cfg?.done_when ?? "");

  const budgetWarning = trigKind !== "manual" && (!budget.trim() || Number(budget) <= 0);

  async function handleSaveSettings() {
    if (!settingsAreValid(trigKind, trigValue, deadline)) return;
    setBusy("fleet_set_loop");
    try {
      await invoke("fleet_set_loop", {
        name: detail.name,
        trigger: buildTrigger(trigKind, trigValue),
        maxIterations: maxIter.trim() ? Number(maxIter) : null,
        deadline: deadline.trim() || null,
        budgetUsd: budget.trim() ? Number(budget) : null,
        doneWhen: doneWhen.trim() || null,
      });
      showToast(t("fleet.settings.saved"));
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  async function saveLabels(ids: string[]) {
    setBusy("fleet_set_labels");
    try {
      await invoke("fleet_set_labels", { name: detail.name, ids });
      onRefresh(); // reloads the list so the rail regroups immediately
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

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

  const [worktree, setWorktree] = useState(false);
  const [loopOpen, setLoopOpen] = useState(false);
  const [loopIterations, setLoopIterations] = useState("");
  const [loopDeadline, setLoopDeadline] = useState("");
  const [loopBudget, setLoopBudget] = useState("");

  function toggleLoopPanel() {
    if (!loopOpen) {
      setLoopIterations(detail.loop_cfg?.max_iterations ? String(detail.loop_cfg.max_iterations) : "");
      setLoopDeadline(detail.loop_cfg?.deadline ?? "");
      setLoopBudget(detail.loop_cfg?.budget_usd ? String(detail.loop_cfg.budget_usd) : "");
    }
    setLoopOpen((v) => !v);
  }

  async function handleRunLoop() {
    if (!loopDeadlineIsValid(loopDeadline)) return;
    showToast(t("fleet.runStarted"));
    await call("fleet_run_loop", {
      name: detail.name,
      maxIterations: loopIterations.trim() ? Number(loopIterations) : null,
      deadline: loopDeadline.trim() || null,
      budgetUsd: loopBudget.trim() ? Number(loopBudget) : null,
    });
  }

  async function handleRun() {
    showToast(t("fleet.runStarted"));
    await call("fleet_run", { name: detail.name, worktree });
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
    const dest = await save({
      defaultPath: `${detail.name}.fleet`,
      filters: [{ name: "Fleet Bundle", extensions: ["fleet"] }],
    });
    if (!dest) return;
    setBusy("fleet_export");
    try {
      await invoke("fleet_export_to", { name: detail.name, path: dest });
      showToast(t("fleet.exported").replace("{path}", dest), 4000);
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
          {modeBadgeLabel(detail.parallel_summary, t) && (
            <span className="fleet-detail__mode-badge">{modeBadgeLabel(detail.parallel_summary, t)}</span>
          )}
        </div>
        <p className="fleet-detail__goal">{detail.goal}</p>
        <p className="fleet-detail__router">{t("fleet.router")}: {detail.router}</p>
      </div>

      {/* Primary action */}
      <div className="fleet-detail__run">
        {detail.parallel_summary && (
          <label className="fleet-detail__worktree-toggle">
            <input
              type="checkbox"
              checked={worktree}
              onChange={(e) => setWorktree(e.target.checked)}
            />
            {t("fleet.run.worktree")}
          </label>
        )}
        <div className="fleet-detail__run-buttons">
          <button className="toolbar-btn toolbar-btn--primary" onClick={handleRun} disabled={busy !== null}>
            ▶ {t("fleet.run")}
          </button>
          <button className="toolbar-btn" onClick={toggleLoopPanel} disabled={busy !== null}>
            {t("fleet.run.loop")} {loopOpen ? "▴" : "▾"}
          </button>
        </div>
        {loopOpen && (
          <>
            <div className="fleet-detail__loop-row">
              <input
                value={loopIterations}
                onChange={(e) => setLoopIterations(e.target.value)}
                placeholder="8"
              />
              <input
                value={loopDeadline}
                onChange={(e) => setLoopDeadline(e.target.value)}
                placeholder="2h"
              />
              <input value={loopBudget} onChange={(e) => setLoopBudget(e.target.value)} placeholder="$" />
              <button
                className="toolbar-btn toolbar-btn--primary"
                onClick={handleRunLoop}
                disabled={busy !== null || !loopDeadlineIsValid(loopDeadline)}
              >
                {t("fleet.run.go")}
              </button>
            </div>
            {!loopDeadlineIsValid(loopDeadline) && (
              <div className="fleet-settings__warning">{t("fleet.settings.invalidDuration")}</div>
            )}
          </>
        )}
      </div>

      {/* Management: Start/Stop · Export · Import */}
      <div className="fleet-detail__mgmt">
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
      </div>

      <div className="fleet-section">
        <div className="fleet-section__label">{t("fleet.labels")}</div>
        {labels.length === 0 ? (
          <div className="fleet-labels__empty">{t("fleet.labelsEmpty")}</div>
        ) : (
          <>
            <div className="fleet-labels">
              {labels.map((l) => {
                const on = fleetLabels.includes(l.id);
                const primary = fleetLabels[0] === l.id;
                return (
                  <button
                    key={l.id}
                    className={`fleet-chip${on ? " is-active" : ""}${primary ? " is-primary" : ""}`}
                    style={l.color ? { borderColor: l.color } : undefined}
                    disabled={busy !== null}
                    title={primary ? t("fleet.labelPrimary") : t("fleet.labelMakePrimary")}
                    onClick={(e) => {
                      // Plain click toggles; alt/⌥-click promotes to primary.
                      const next = e.altKey
                        ? makePrimary(fleetLabels, l.id)
                        : toggleAssignment(fleetLabels, l.id);
                      void saveLabels(next);
                    }}
                  >
                    {primary && <span className="fleet-chip__pin">★</span>}
                    {l.display || l.id}
                  </button>
                );
              })}
            </div>
            <div className="fleet-labels__hint">{t("fleet.labelHint")}</div>
          </>
        )}
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
        <div className="fleet-section__label">{t("fleet.settings.title")}</div>
        <div className="fleet-settings__row">
          <label>{t("fleet.settings.trigger")}</label>
          <select value={trigKind} onChange={(e) => setTrigKind(e.target.value as TriggerKind)}>
            <option value="manual">{t("fleet.settings.triggerManual")}</option>
            <option value="interval">{t("fleet.settings.triggerInterval")}</option>
            <option value="cron">{t("fleet.settings.triggerCron")}</option>
          </select>
          {trigKind !== "manual" && (
            <input
              value={trigValue}
              onChange={(e) => setTrigValue(e.target.value)}
              placeholder={trigKind === "interval" ? "30m" : "*/15 * * * *"}
            />
          )}
        </div>
        {trigKind === "interval" && !DURATION_RE.test(trigValue.trim()) && (
          <div className="fleet-settings__warning">{t("fleet.settings.invalidDuration")}</div>
        )}
        <div className="fleet-settings__row">
          <label>{t("fleet.settings.maxIterations")}</label>
          <input value={maxIter} onChange={(e) => setMaxIter(e.target.value)} placeholder="8" />
        </div>
        <div className="fleet-settings__row">
          <label>{t("fleet.settings.deadline")}</label>
          <input value={deadline} onChange={(e) => setDeadlineValue(e.target.value)} placeholder="2h" />
        </div>
        {deadline.trim() !== "" && !DURATION_RE.test(deadline.trim()) && (
          <div className="fleet-settings__warning">{t("fleet.settings.invalidDuration")}</div>
        )}
        <div className="fleet-settings__hint">{t("fleet.settings.deadlineHint")}</div>
        <div className="fleet-settings__row">
          <label>{t("fleet.settings.budget")}</label>
          <input value={budget} onChange={(e) => setBudget(e.target.value)} placeholder="0.00" />
        </div>
        {budgetWarning && <div className="fleet-settings__warning">{t("fleet.settings.budgetWarning")}</div>}
        <div className="fleet-settings__row">
          <label>{t("fleet.settings.doneWhen")}</label>
          <input
            value={doneWhen}
            onChange={(e) => setDoneWhen(e.target.value)}
            placeholder={t("fleet.settings.doneWhenHint")}
          />
        </div>
        <div className="fleet-settings__hint">
          {t("fleet.settings.lastRun")}: {detail.loop_cfg?.last_run ?? t("fleet.settings.lastRunNever")}
          <br />
          {t("fleet.settings.stopHint")}
        </div>
        <button
          className="toolbar-btn toolbar-btn--primary"
          onClick={handleSaveSettings}
          disabled={busy !== null || !settingsAreValid(trigKind, trigValue, deadline)}
        >
          {t("fleet.settings.save")}
        </button>
      </div>

      <div className="fleet-section fleet-section--jobs">
        <div className="fleet-section__header">
          <span className="fleet-section__label">{t("fleet.jobs")}</span>
          {jobs.filter((j) => !["done", "failed", "canceled"].includes(j.status)).length > 0 && (
            <span className="fleet-section__badge">
              {jobs.filter((j) => !["done", "failed", "canceled"].includes(j.status)).length}
            </span>
          )}
        </div>
        <div className="fleet-send fleet-send--inset">
          <input
            value={sendInput}
            onChange={(e) => setSendInput(e.target.value)}
            placeholder={t("fleet.sendPlaceholder")}
            onKeyDown={(e) => e.key === "Enter" && handleSend()}
          />
          <button className="toolbar-btn toolbar-btn--primary" onClick={handleSend} disabled={busy !== null || !sendInput.trim()}>
            {t("fleet.send")}
          </button>
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
            </div>
          ))}
        </div>
        <button className="fleet-jobs__more" onClick={handleShowAll} disabled={busy !== null}>
          {showAll ? t("fleet.showActive") : t("fleet.showAll")}
        </button>
      </div>

      {/* Danger Zone */}
      <div className="fleet-detail__danger">
        <div className="fleet-detail__danger-label">{t("fleet.dangerZone")}</div>
        <div className="fleet-detail__danger-row">
          <span className="fleet-detail__danger-desc">{t("fleet.deleteDesc")}</span>
          <button className="toolbar-btn toolbar-btn--danger" onClick={handleDelete} disabled={busy !== null}>
            {t("fleet.delete")}
          </button>
        </div>
      </div>
    </div>
  );
}
