import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { scheduleNext, type ScheduleItem, type ScheduleStatus } from "../../../schedule";
import type { FleetDetail as Detail } from "../../fleet/types";
import { useT } from "../../../i18n";
import { DURATION_RE } from "../../fleet/fleetCreateForm";
import {
  parseTrigger,
  buildTrigger,
  settingsAreValid,
  parseDonePolicy,
  buildDoneWhen,
  DONE_POLICY_HINT,
  DONE_WHEN_QUEUE_EMPTY,
  buildCronExpr,
  CRON_PREVIEW_COUNT,
  CRON_PREVIEW_DEBOUNCE_MS,
  parseCronTime,
  CRON_DEFAULT_TIME,
  type TriggerKind,
  type DonePolicyKind,
  type CronShape,
} from "../../fleet/fleetSettingsForm";
import { deleteFleet, showToast, useFleetCall } from "./fleetActions";

export interface FleetSettingsProps {
  detail: Detail;
  onRefresh: () => void;
  onDelete: () => void;
}

/** Settings tab (spec §4.4): trigger / cron / loop guards / done-when, with
 *  the Danger zone last. State and effects are the old FleetDetail's. */
export function FleetSettings({ detail, onRefresh, onDelete }: FleetSettingsProps) {
  const { t } = useT();
  const { busy, setBusy } = useFleetCall(onRefresh);

  const initialTrigger = parseTrigger(detail.loop_cfg);
  const [trigKind, setTrigKind] = useState<TriggerKind>(initialTrigger.kind);
  const [trigValue, setTrigValue] = useState(initialTrigger.value);
  const [cronShape, setCronShape] = useState<CronShape>("custom");
  const [cronTime, setCronTime] = useState(parseCronTime(initialTrigger.value) ?? CRON_DEFAULT_TIME);
  const [cronFires, setCronFires] = useState<string[] | null>(null);
  const [cronInvalid, setCronInvalid] = useState(false);
  const cronRequestId = useRef(0);

  // Ask the backend what this expression will actually do. Debounced so typing
  // does not fire a command per keystroke; the cleanup cancels an in-flight
  // timer so only the latest value is ever evaluated. A request sequence
  // number guards the case where the timer already fired and invoke() is in
  // flight: two edits close together can resolve out of order (next_n_fires'
  // cost varies with how far it has to scan), so only the response matching
  // the latest dispatched request is applied.
  useEffect(() => {
    if (trigKind !== "cron" || trigValue.trim() === "") {
      setCronFires(null);
      setCronInvalid(false);
      return;
    }
    const timer = setTimeout(() => {
      const requestId = ++cronRequestId.current;
      invoke<string[]>("cron_preview", { expr: trigValue, count: CRON_PREVIEW_COUNT })
        .then((fires) => {
          if (cronRequestId.current !== requestId) return;
          setCronFires(fires);
          setCronInvalid(false);
        })
        .catch(() => {
          if (cronRequestId.current !== requestId) return;
          setCronFires(null);
          setCronInvalid(true);
        });
    }, CRON_PREVIEW_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [trigKind, trigValue]);

  // What the fleet is actually set up to do, as opposed to what the unsaved
  // form says. Read from the same aggregator the Panel and the agent detail
  // use, so the three cannot answer "when does this next fire" differently.
  const [saved, setSaved] = useState<ScheduleItem | null>(null);
  useEffect(() => {
    let live = true;
    void invoke<ScheduleStatus>("panel_schedule_status", { agent: null })
      .then((st) => {
        if (!live) return;
        setSaved(
          st.schedules.find((r) => r.kind === "fleet" && r.owner === detail.name) ?? null,
        );
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [detail.name, detail.loop_cfg?.trigger, detail.loop_cfg?.last_run]);

  const [maxIter, setMaxIter] = useState(
    detail.loop_cfg?.max_iterations ? String(detail.loop_cfg.max_iterations) : "",
  );
  const [deadline, setDeadlineValue] = useState(detail.loop_cfg?.deadline ?? "");
  const [budget, setBudget] = useState(
    detail.loop_cfg?.budget_usd ? String(detail.loop_cfg.budget_usd) : "",
  );
  const loadedDoneWhen = detail.loop_cfg?.done_when ?? "";
  const loadedDonePolicy = parseDonePolicy(loadedDoneWhen);
  const [donePolicy, setDonePolicy] = useState<DonePolicyKind>(loadedDonePolicy);

  const budgetWarning = trigKind !== "manual" && (!budget.trim() || Number(budget) <= 0);

  async function handleSaveSettings() {
    if (!settingsAreValid(trigKind, trigValue, deadline)) return;
    setBusy("fleet_set_loop");
    try {
      await invoke("fleet_set_loop", {
        name: detail.name,
        trigger: buildTrigger(trigKind, trigValue),
        maxIterations: maxIter.trim() ? Math.trunc(Number(maxIter)) : null,
        // Always a string, never null: the backend reads a `null` deadline as
        // "leave this field alone", so `|| null` here was the same
        // can't-clear-the-field bug `buildDoneWhen` already fixed for
        // `done_when` -- an emptied box would save, say "Settings saved", and
        // silently keep the old deadline. The validator already accepts "".
        deadline: deadline.trim(),
        budgetUsd: budget.trim() ? Number(budget) : null,
        doneWhen: buildDoneWhen(donePolicy, loadedDoneWhen),
      });
      showToast(t("fleet.settings.saved"));
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  function applyCronShape(shape: CronShape, time: string) {
    setCronShape(shape);
    setCronTime(time);
    if (shape !== "custom") setTrigValue(buildCronExpr(shape, time));
  }

  function handleDelete() {
    void deleteFleet(
      detail,
      { confirm: t("fleet.confirmDelete").replace("{name}", detail.display_name), title: t("fleet.delete") },
      setBusy,
      onDelete,
    );
  }

  return (
    <>
      <section className="detail-section" id="fleet-settings">
        <h3 className="detail-section__title">{t("fleet.settings.title")}</h3>
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
        {trigKind === "cron" && (
          <div className="fleet-settings__row">
            <label>{t("fleet.settings.cronShape")}</label>
            <select
              value={cronShape}
              onChange={(e) => applyCronShape(e.target.value as CronShape, cronTime)}
            >
              <option value="custom">{t("fleet.settings.cronShapeCustom")}</option>
              <option value="hourly">{t("fleet.settings.cronShapeHourly")}</option>
              <option value="daily">{t("fleet.settings.cronShapeDaily")}</option>
              <option value="weekdays">{t("fleet.settings.cronShapeWeekdays")}</option>
            </select>
            {cronShape !== "custom" && (
              <input
                type="time"
                value={cronTime}
                onChange={(e) => applyCronShape(cronShape, e.target.value)}
              />
            )}
          </div>
        )}
        {trigKind === "cron" && cronInvalid && (
          <div className="fleet-settings__warning">{t("fleet.settings.cronInvalid")}</div>
        )}
        {trigKind === "cron" && cronFires !== null && cronFires.length === 0 && (
          <div className="fleet-settings__warning">{t("fleet.settings.cronNeverFires")}</div>
        )}
        {trigKind === "cron" && cronFires !== null && cronFires.length > 0 && (
          <div className="fleet-settings__hint">
            {t("fleet.settings.cronNext")}: {cronFires.join(" · ")} ({t("fleet.settings.cronLocalTime")})
          </div>
        )}
        <div className="fleet-settings__row">
          <label>{t("fleet.settings.maxIterations")}</label>
          <input
            type="number"
            min="1"
            step="1"
            value={maxIter}
            onChange={(e) => setMaxIter(e.target.value)}
            placeholder="8"
          />
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
          <input
            type="number"
            min="0"
            step="0.01"
            value={budget}
            onChange={(e) => setBudget(e.target.value)}
            placeholder="0.00"
          />
        </div>
        {budgetWarning && <div className="fleet-settings__warning">{t("fleet.settings.budgetWarning")}</div>}
        <div className="fleet-settings__row">
          <label>{t("fleet.settings.doneWhen")}</label>
          <select
            value={donePolicy}
            onChange={(e) => setDonePolicy(e.target.value as DonePolicyKind)}
          >
            <option value="router">{t("fleet.settings.donePolicyRouter")}</option>
            <option value={DONE_WHEN_QUEUE_EMPTY}>{t("fleet.settings.donePolicyQueueEmpty")}</option>
            {/* Only offered when one is already set: the Hub preserves a marker
                but never authors one, because it cannot supply the half of the
                contract that teaches an agent to emit the text. */}
            {loadedDonePolicy === "marker" && (
              <option value="marker">{loadedDoneWhen.trim()}</option>
            )}
          </select>
        </div>
        <div className="fleet-settings__hint">{t(DONE_POLICY_HINT[donePolicy])}</div>
        <div className="fleet-settings__hint">
          {t("fleet.settings.lastRun")}: {detail.loop_cfg?.last_run ?? t("fleet.settings.lastRunNever")}
          <br />
          {/* Rendered only once the aggregator has answered: before that there
              is nothing true to say, and a placeholder here would be read as
              one. */}
          {saved && (
            <>
              {t("fleet.settings.nextRun")}: {scheduleNext(saved).text}
              <br />
            </>
          )}
          {t("fleet.settings.stopHint")}
        </div>
        <button
          className="btn btn--primary"
          onClick={handleSaveSettings}
          disabled={busy !== null || !settingsAreValid(trigKind, trigValue, deadline)}
        >
          {t("fleet.settings.save")}
        </button>
      </section>

      <section className="detail-section fleet-detail__danger" id="fleet-danger">
        <div className="fleet-detail__danger-label">{t("fleet.dangerZone")}</div>
        <div className="fleet-detail__danger-row">
          <span className="fleet-detail__danger-desc">{t("fleet.deleteDesc")}</span>
          <button className="toolbar-btn toolbar-btn--danger" onClick={handleDelete} disabled={busy !== null}>
            {t("fleet.delete")}
          </button>
        </div>
      </section>
    </>
  );
}
