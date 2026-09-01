import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AgentDetail, DetailPatch, NotifConfig, NotifPatch } from "../../../types";
import { useT } from "../../../i18n";
import type { TranslationKey } from "../../../i18n/types";

const BEHAVIOR_OPTIONS: { id: string; labelKey: TranslationKey; descKey: TranslationKey }[] = [
  { id: "quiet", labelKey: "detail.quiet", descKey: "detail.quietDesc" },
  { id: "normal", labelKey: "detail.normal", descKey: "detail.normalDesc" },
  { id: "lively", labelKey: "detail.lively", descKey: "detail.livelyDesc" },
];

export function BehaviorTab({
  detail,
  onSaved,
}: {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
}) {
  const { t } = useT();
  const [selected, setSelected] = useState(detail.behavior_preset);
  const [effort, setEffort] = useState(detail.effort);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  async function pick(b: string) {
    setSelected(b);
    setSaving(true);
    setSaveError(null);
    try {
      const updated = await invoke<AgentDetail>("update_agent_detail", {
        name: detail.agent_name,
        patch: { behavior_preset: b } as DetailPatch,
      });
      onSaved(updated);
    } catch (e) {
      setSaveError(String(e));
    } finally {
      setSaving(false);
    }
  }

  // NOTE: deliberately no EFFORT_OPTIONS constant beside BEHAVIOR_OPTIONS
  // above. The behavior presets are a fixed set MUR owns; effort levels are a
  // property of the agent's MODEL, and a hardcoded list would offer `medium`
  // on deepseek-v4 (which has none) and `xhigh` on pre-4.7 Claude (a 400).
  // The backend resolves them via `effort_shape` and sends `effort_levels`.
  async function pickEffort(level: string) {
    setEffort(level);
    setSaving(true);
    setSaveError(null);
    try {
      const updated = await invoke<AgentDetail>("update_agent_detail", {
        name: detail.agent_name,
        patch: { effort: level } as DetailPatch,
      });
      // Take the value the backend reports rather than the one clicked: a
      // level the model cannot use comes back narrowed.
      setEffort(updated.effort);
      onSaved(updated);
    } catch (e) {
      setSaveError(String(e));
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="tab-form">
      <p className="field-muted" style={{ marginBottom: 12, fontSize: 12 }}>
        {t("detail.behaviorHint")}
      </p>
      {BEHAVIOR_OPTIONS.map((opt) => (
        <label
          key={opt.id}
          className={`radio-card${selected === opt.id ? " radio-card--active" : ""}${saving ? " radio-card--disabled" : ""}`}
        >
          <input
            type="radio"
            name="behavior"
            value={opt.id}
            checked={selected === opt.id}
            onChange={() => pick(opt.id)}
            disabled={saving}
            style={{ display: "none" }}
          />
          <div className="radio-card-label">{t(opt.labelKey)}</div>
          <div className="radio-card-desc">{t(opt.descKey)}</div>
        </label>
      ))}
      {detail.effort_levels.length > 0 && (
        <>
          <div className="notif-section__title">{t("detail.effort")}</div>
          <p className="field-muted" style={{ marginBottom: 12, fontSize: 12 }}>
            {t("detail.effortHint")}
          </p>
          {detail.effort_stored && detail.effort_stored !== detail.effort && (
            <p className="field-muted" style={{ marginBottom: 12, fontSize: 12 }}>
              {t("detail.effortNarrowed")
                .replace("{stored}", detail.effort_stored)
                .replace("{using}", detail.effort ?? "")}
            </p>
          )}
          {detail.effort_levels.map((level) => (
            <label
              key={level}
              className={`radio-card${effort === level ? " radio-card--active" : ""}${saving ? " radio-card--disabled" : ""}`}
            >
              <input
                type="radio"
                name="effort"
                value={level}
                checked={effort === level}
                disabled={saving}
                onChange={() => pickEffort(level)}
              />
              <div className="radio-card-label">{level}</div>
            </label>
          ))}
        </>
      )}
      {saving && <p className="field-muted" style={{ fontSize: 12 }}>{t("detail.saving")}</p>}
      {saveError && <p className="save-error">{saveError}</p>}
      <NotificationsSection agentName={detail.agent_name} />
    </div>
  );
}

function NotificationsSection({ agentName }: { agentName: string }) {
  const { t } = useT();
  const [cfg, setCfg] = useState<NotifConfig | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<NotifConfig>("agent_get_notif_config", { name: agentName })
      .then(setCfg)
      .catch((e) => setError(String(e)));
  }, [agentName]);

  async function patch(p: NotifPatch) {
    try {
      const updated = await invoke<NotifConfig>("agent_set_notif_config", {
        name: agentName,
        patch: p,
      });
      setCfg(updated);
    } catch (e) {
      setError(String(e));
    }
  }

  if (!cfg) return null;

  return (
    <div className="notif-section">
      <div className="notif-section__title">{t("detail.notifications")}</div>

      <label className="notif-row">
        <span>{t("detail.proactiveMessages")}</span>
        <input
          type="checkbox"
          className="notif-toggle"
          checked={cfg.enabled}
          onChange={(e) => patch({ enabled: e.target.checked })}
        />
      </label>

      <label className="notif-row">
        <span>
          {t("detail.dailyCap")} <b>{cfg.daily_cap}</b>
        </span>
        <input
          type="range"
          min={0}
          max={20}
          step={1}
          value={cfg.daily_cap}
          className="notif-slider"
          onChange={(e) => patch({ daily_cap: Number(e.target.value) })}
        />
      </label>

      <label className="notif-row">
        <span>{t("detail.quietHours")}</span>
        <input
          type="checkbox"
          className="notif-toggle"
          checked={cfg.quiet_hours_enabled}
          onChange={(e) => patch({ quiet_hours_enabled: e.target.checked })}
        />
      </label>

      <div
        className={`notif-times${cfg.quiet_hours_enabled ? "" : " notif-times--disabled"}`}
      >
        <div className="notif-time">
          <span className="notif-time__label">{t("detail.quietFrom")}</span>
          <input
            type="time"
            value={cfg.quiet_start}
            disabled={!cfg.quiet_hours_enabled}
            onChange={(e) => patch({ quiet_start: e.target.value })}
          />
        </div>
        <div className="notif-time">
          <span className="notif-time__label">{t("detail.quietUntil")}</span>
          <input
            type="time"
            value={cfg.quiet_end}
            disabled={!cfg.quiet_hours_enabled}
            onChange={(e) => patch({ quiet_end: e.target.value })}
          />
        </div>
      </div>

      {error && <p className="save-error">{error}</p>}
    </div>
  );
}
