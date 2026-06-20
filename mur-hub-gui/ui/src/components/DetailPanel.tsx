//! Detail panel — right-side slide-in when an agent is selected in the
//! dashboard. Provides 7 tabs: Persona, Style, Behavior, Skills, MCP,
//! Permissions, Inbox.

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import type { AgentEntry } from "../types";
import {
  ALL_DETAIL_TABS,
  BUILTIN_PRESETS,
  type AgentDetail,
  type DetailPatch,
  type DetailTab,
  type NotifConfig,
  type NotifPatch,
  type SkillInstallResult,
} from "../types";
import { useAgents } from "../context/AgentContext";
import { CompanionInbox } from "./CompanionInbox";
import { ModelCombobox } from "./ModelCombobox";
import { ModelLibrary } from "./ModelLibrary";
import { MobileTab } from "./MobileTab";
import { MemoryTab } from "./MemoryTab";
import { useT } from "../i18n";
import type { TranslationKey } from "../i18n/types";
import { CATEGORY_COLORS, TAB_ICONS, avatarInitials } from "../utils";

// Tab → i18n key map (replaces the hardcoded TAB_LABELS lookup).
const TAB_LABEL_KEYS: Record<DetailTab, TranslationKey> = {
  persona: "detail.persona",
  style: "detail.style",
  behavior: "detail.behavior",
  skills: "detail.skills",
  mcp: "detail.mcp",
  permissions: "detail.permissions",
  inbox: "detail.inbox",
  mobile: "detail.mobile",
  memory: "detail.memory",
};

interface Props {
  agentName: string;
  agents: AgentEntry[];
  onClose: () => void;
}

// Lightweight toast — appends a bare `.toast` element to <body>, mirrors
// the feedback pattern in DashboardApp (its showToast is module-local there).
function showToast(msg: string, durationMs = 2000) {
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = msg;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), durationMs);
}

export function DetailPanel({ agentName, agents, onClose }: Props) {
  const { t } = useT();
  const { desiredDetailTab, setDesiredDetailTab } = useAgents();
  const [detail, setDetail] = useState<AgentDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<DetailTab>("persona");
  const [libraryOpen, setLibraryOpen] = useState(false);

  useEffect(() => {
    setError(null);
    setActiveTab("persona");
    invoke<AgentDetail>("get_agent_detail", { name: agentName })
      .then(setDetail)
      .catch((e) => setError(String(e)));
  }, [agentName]);

  // Honor a requested deep-link tab (e.g. 🎨 on an agent card → Style), then
  // clear it so it fires once. Runs after the per-agent reset above, so it wins.
  useEffect(() => {
    if (!desiredDetailTab) return;
    if ((ALL_DETAIL_TABS as readonly string[]).includes(desiredDetailTab)) {
      setActiveTab(desiredDetailTab as DetailTab);
    }
    setDesiredDetailTab(null);
  }, [desiredDetailTab, setDesiredDetailTab]);

  const entry = agents.find((a) => a.name === agentName);
  const displayName = entry?.display_name ?? agentName;
  const status = entry?.status ?? "idle";
  const isRunning = status === "running";

  function handleSaved(updated: AgentDetail) {
    setDetail(updated);
  }

  function handleRun(name: string) {
    invoke("start_agent", { name }).catch((e) => showToast(`Failed: ${e}`));
  }
  function handleStop(name: string) {
    invoke("stop_agent", { name }).catch((e) => showToast(`Failed: ${e}`));
  }
  async function handleExport(name: string) {
    const outPath = await save({
      defaultPath: `${name}.muragent`,
      filters: [{ name: "MUR Agent", extensions: ["muragent"] }],
    }).catch((e) => {
      showToast(`Export failed: ${e}`, 6000);
      return null;
    });
    if (!outPath) return;
    invoke<string>("export_muragent_file", { name, outPath })
      .then(() => showToast(`Exported ${name}.muragent`))
      .catch((e) => showToast(`Export failed: ${e}`, 6000));
  }

  function Header({ name }: { name: string }) {
    const avatarColor = CATEGORY_COLORS[entry?.category ?? "custom"] ?? "#64748B";
    return (
      <div className="detail-panel__header">
        <div className="detail-panel__top">
          <div
            className="detail-panel__avatar"
            style={{ background: avatarColor, color: "#fff", fontSize: "18px", fontWeight: 700 }}
          >
            {avatarInitials(name)}
          </div>
          <div className="detail-panel__ident">
            <div className="detail-panel__name">{name}</div>
            <span className={`pill pill--${isRunning ? "run" : "idle"}`}>
              <span className="pill__dot" />
              {t(isRunning ? "status.running" : "status.idle")}
            </span>
          </div>
          <button
            className="detail-panel__close"
            onClick={onClose}
            title={t("detail.close")}
            aria-label={t("detail.close")}
          >
            ×
          </button>
        </div>
        <div className="detail-panel__actions">
          {isRunning ? (
            <button
              className="btn btn--sm btn--danger"
              onClick={() => handleStop(agentName)}
            >
              {t("action.stop")}
            </button>
          ) : (
            <button
              className="btn btn--sm btn--primary"
              onClick={() => handleRun(agentName)}
            >
              {t("action.run")}
            </button>
          )}
          <button
            className="btn btn--sm btn--secondary"
            onClick={() => handleExport(agentName)}
          >
            {t("action.export")}
          </button>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <aside className="detail-panel">
        <Header name={displayName} />
        <div className="detail-panel__body">
          <p className="detail-error">{t("detail.loadFailed", { error })}</p>
        </div>
      </aside>
    );
  }

  if (!detail) {
    return (
      <aside className="detail-panel">
        <Header name={displayName} />
        <div className="detail-panel__body">
          <p className="detail-loading">{t("detail.loading")}</p>
        </div>
      </aside>
    );
  }

  return (
    <aside className="detail-panel">
      <Header name={detail.display_name} />
      <div className="detail-panel-tabs">
        {ALL_DETAIL_TABS.map((tab) => (
          <span
            key={tab}
            className={`detail-tab${activeTab === tab ? " detail-tab--active" : ""}`}
            onClick={() => setActiveTab(tab)}
            title={t(TAB_LABEL_KEYS[tab])}
          >
            <span className="detail-tab__icon">{TAB_ICONS[tab]}</span>
            <span className="detail-tab__label">{t(TAB_LABEL_KEYS[tab])}</span>
          </span>
        ))}
      </div>
      <div className="detail-panel__body">
        {activeTab === "persona" && (
          <>
            <ModelCombobox
              detail={detail}
              onSaved={handleSaved}
              onManage={() => setLibraryOpen(true)}
            />
            <ModelLibrary open={libraryOpen} onClose={() => setLibraryOpen(false)} />
            <PersonaTab detail={detail} onSaved={handleSaved} />
          </>
        )}
        {activeTab === "style" && (
          <StyleTab detail={detail} onSaved={handleSaved} />
        )}
        {activeTab === "behavior" && (
          <BehaviorTab detail={detail} onSaved={handleSaved} />
        )}
        {activeTab === "skills" && (
          <SkillsTab detail={detail} onSaved={handleSaved} />
        )}
        {activeTab === "mcp" && <McpTab detail={detail} onSaved={handleSaved} />}
        {activeTab === "permissions" && <PermissionsTab detail={detail} />}
        {activeTab === "inbox" && <CompanionInbox agentName={agentName} />}
        {activeTab === "mobile" && <MobileTab agentName={agentName} />}
        {activeTab === "memory" && <MemoryTab agentName={agentName} />}
      </div>
    </aside>
  );
}

// ModelSection replaced by ModelCombobox (S3 Task 5).

// ─── Persona Tab ──────────────────────────────────────────────────────────

const PERSONA_CATEGORIES = [
  "research", "automation", "monitor", "notify", "commerce", "custom",
];

const TONE_OPTIONS = ["professional", "casual", "friendly", "direct", "playful", "formal"];
const RISK_OPTIONS = ["conservative", "balanced", "bold"];
const VERBOSITY_OPTIONS = ["concise", "balanced", "detailed"];

// Include the agent's current value as an option even when it isn't one of the
// canned choices (older agents / custom vocab), so the <select> shows the real
// value instead of silently snapping to the first option — which misrepresents
// the persona and risks clobbering the value on save.
function withCurrent(options: string[], current: string): string[] {
  return current && !options.includes(current) ? [current, ...options] : options;
}

function PersonaTab({
  detail,
  onSaved,
}: {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
}) {
  const { t } = useT();
  const [category, setCategory] = useState(detail.persona_category);
  const [description, setDescription] = useState(detail.persona_description);
  const [tone, setTone] = useState(detail.persona_tone);
  const [risk, setRisk] = useState(detail.persona_risk);
  const [verbosity, setVerbosity] = useState(detail.persona_verbosity);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);

  async function save() {
    setSaving(true);
    setSaveError(null);
    try {
      const patch: DetailPatch = {
        persona_category: category,
        persona_description: description,
        persona_tone: tone,
        persona_risk: risk,
        persona_verbosity: verbosity,
      };
      const updated = await invoke<AgentDetail>("update_agent_detail", {
        name: detail.agent_name,
        patch,
      });
      onSaved(updated);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } catch (e) {
      setSaveError(String(e));
    } finally {
      setSaving(false);
    }
  }

  const changed =
    category !== detail.persona_category ||
    description !== detail.persona_description ||
    tone !== detail.persona_tone ||
    risk !== detail.persona_risk ||
    verbosity !== detail.persona_verbosity;

  // Visual meter level for risk / verbosity (0-2 → 1-3 bars on).
  const riskLevel = Math.max(0, RISK_OPTIONS.indexOf(risk));
  const verbosityLevel = Math.max(0, VERBOSITY_OPTIONS.indexOf(verbosity));

  return (
    <div className="tab-form">
      <label className="field-label">{t("detail.category")}</label>
      <select
        className="input"
        value={category}
        onChange={(e) => { setCategory(e.target.value); }}
      >
        {PERSONA_CATEGORIES.map((c) => (
          <option key={c} value={c}>{c}</option>
        ))}
      </select>

      <label className="field-label">{t("detail.description")}</label>
      <textarea
        className="input"
        rows={3}
        value={description}
        onChange={(e) => { setDescription(e.target.value); }}
        placeholder={t("detail.descPlaceholder")}
      />

      <label className="field-label">{t("detail.tone")}</label>
      <select
        className="input"
        value={tone}
        onChange={(e) => { setTone(e.target.value); }}
      >
        {withCurrent(TONE_OPTIONS, tone).map((opt) => (
          <option key={opt} value={opt}>{opt}</option>
        ))}
      </select>

      <label className="field-label">{t("detail.risk")}</label>
      <div className="meter">
        {[0, 1, 2].map((i) => (
          <i key={i} className={i <= riskLevel ? "is-on" : ""} />
        ))}
      </div>
      <select
        className="input"
        value={risk}
        onChange={(e) => { setRisk(e.target.value); }}
      >
        {withCurrent(RISK_OPTIONS, risk).map((r) => (
          <option key={r} value={r}>{r}</option>
        ))}
      </select>

      <label className="field-label">{t("detail.verbosity")}</label>
      <div className="meter">
        {[0, 1, 2].map((i) => (
          <i key={i} className={i <= verbosityLevel ? "is-on" : ""} />
        ))}
      </div>
      <select
        className="input"
        value={verbosity}
        onChange={(e) => { setVerbosity(e.target.value); }}
      >
        {withCurrent(VERBOSITY_OPTIONS, verbosity).map((v) => (
          <option key={v} value={v}>{v}</option>
        ))}
      </select>

      <button
        className="btn btn--primary btn--sm save-btn"
        disabled={!changed || saving}
        onClick={save}
      >
        {saving ? t("detail.saving") : saved ? t("detail.saved") : t("detail.save")}
      </button>
      {saveError && <p className="save-error">{saveError}</p>}
    </div>
  );
}

// ─── Style Tab ────────────────────────────────────────────────────────────

function StyleTab({
  detail,
  onSaved,
}: {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
}) {
  const { t } = useT();
  const [selected, setSelected] = useState(detail.style_preset);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [rendering, setRendering] = useState(detail.render_status.status === "rendering");

  // Poll for render completion while a render is in flight, refreshing the
  // panel so the status text/progress and preset thumbnails stay current.
  useEffect(() => {
    if (!rendering) return;
    let cancelled = false;
    const started = Date.now();
    const timer = setInterval(async () => {
      try {
        const fresh = await invoke<AgentDetail>("get_agent_detail", {
          name: detail.agent_name,
        });
        if (cancelled) return;
        onSaved(fresh);
        if (fresh.render_status.status === "ready" || fresh.render_status.status === "failed") {
          setRendering(false);
        }
      } catch {
        // transient; keep polling
      }
      if (Date.now() - started > 120_000) setRendering(false); // safety timeout
    }, 1500);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [rendering, detail.agent_name, onSaved]);

  // Whether real AI art exists. When false the pet shows the built-in vector
  // mascot, and we surface a gentle "connect an image model" hint.
  const [hasAiArt, setHasAiArt] = useState(true);
  useEffect(() => {
    invoke<{ has_ai_art: boolean }>("pet_get_appearance", { agentName: detail.agent_name })
      .then((a) => setHasAiArt(a.has_ai_art))
      .catch(() => setHasAiArt(false));
  }, [detail.agent_name, detail.render_status]);

  async function triggerRender() {
    setSaveError(null);
    setRendering(true);
    try {
      await invoke("render_agent_expressions", { name: detail.agent_name });
    } catch (e) {
      setSaveError(String(e));
      setRendering(false);
    }
  }

  async function pickPreset(id: string) {
    setSelected(id);
    setSaving(true);
    setSaveError(null);
    try {
      const updated = await invoke<AgentDetail>("update_agent_detail", {
        name: detail.agent_name,
        patch: { style_preset: id } as DetailPatch,
      });
      onSaved(updated);
    } catch (e) {
      setSaveError(String(e));
    } finally {
      setSaving(false);
    }
  }

  function statusText(): string {
    switch (detail.render_status.status) {
      case "pending": return t("detail.notRendered");
      case "rendering": return t("detail.rendering", {
        done: detail.render_status.done,
        total: detail.render_status.total,
      });
      case "ready": return t("detail.renderReady");
      case "failed": return t("detail.renderFailed", { reason: detail.render_status.reason });
    }
  }

  const current = BUILTIN_PRESETS.find((p) => p.id === selected);

  return (
    <div className="tab-form">
      <label className="field-label">{t("detail.currentStyle")}</label>
      <p className="field-value">
        {current?.display_name ?? selected}
        {" "}
        <span className="field-muted">
          ({current?.family ?? t("detail.unknown")})
        </span>
      </p>

      <label className="field-label">{t("detail.renderStatus")}</label>
      <p className="field-muted" style={{ fontSize: 12 }}>{statusText()}</p>
      {detail.render_status.status === "rendering" && (
        <div className="progress-bar" style={{ marginTop: 6 }}>
          <div
            className="progress-fill"
            style={{
              width: `${(detail.render_status.done / detail.render_status.total) * 100}%`,
            }}
          />
        </div>
      )}
      <button
        className={
          detail.render_status.status === "pending"
            ? "btn btn--sm btn--primary"
            : "toolbar-btn"
        }
        style={{ marginTop: 8 }}
        onClick={triggerRender}
        disabled={rendering || detail.render_status.status === "rendering"}
        title={t("detail.renderTooltip")}
      >
        {rendering || detail.render_status.status === "rendering"
          ? t("detail.renderBtnRendering")
          : detail.render_status.status === "ready"
            ? t("detail.renderBtnRerender")
            : t("detail.renderBtnRender")}
      </button>

      {!hasAiArt && (
        <p className="field-muted detail-vector-hint" style={{ fontSize: 12, marginTop: 8 }}>
          {t("detail.vectorHint")}
        </p>
      )}

      <label className="field-label" style={{ marginTop: 16 }}>{t("detail.presetGallery")}</label>
      <div className="style-gallery">
        {BUILTIN_PRESETS.map((p) => (
          <button
            key={p.id}
            className={`style-thumb${selected === p.id ? " is-selected" : ""}`}
            onClick={() => pickPreset(p.id)}
            disabled={saving}
            title={p.description}
          >
            <div className="style-thumb__label">{p.display_name}</div>
            <div className="style-thumb__family">{p.family}</div>
          </button>
        ))}
      </div>
      {saveError && <p className="save-error">{saveError}</p>}
    </div>
  );
}

// ─── Behavior Tab ─────────────────────────────────────────────────────────

const BEHAVIOR_OPTIONS: { id: string; labelKey: TranslationKey; descKey: TranslationKey }[] = [
  { id: "quiet", labelKey: "detail.quiet", descKey: "detail.quietDesc" },
  { id: "normal", labelKey: "detail.normal", descKey: "detail.normalDesc" },
  { id: "lively", labelKey: "detail.lively", descKey: "detail.livelyDesc" },
];

function BehaviorTab({
  detail,
  onSaved,
}: {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
}) {
  const { t } = useT();
  const [selected, setSelected] = useState(detail.behavior_preset);
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

// ─── Skills Tab ───────────────────────────────────────────────────────────

function SkillsTab({
  detail,
  onSaved,
}: {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
}) {
  const { t } = useT();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [installedId, setInstalledId] = useState<string | null>(null);

  const hasInstalled = detail.installed_skills.length > 0;
  const hasLegacy = detail.skills.length > 0;

  async function installSkill() {
    setError(null);
    setInstalledId(null);
    const src = await open({
      multiple: false,
      filters: [{ name: "MUR Skill", extensions: ["yaml", "yml", "md"] }],
    }).catch((e) => {
      setError(String(e));
      return null;
    });
    if (typeof src !== "string" || !src) return;
    setBusy(true);
    try {
      // Backend returns the refreshed detail AND the id the skill was
      // registered as — or rejects with a validation error. Surface the
      // real outcome instead of a blanket "installed" message.
      const res = await invoke<SkillInstallResult>("agent_skill_install", {
        name: detail.agent_name,
        sourcePath: src,
      });
      onSaved(res.detail);
      setInstalledId(res.installed_id);
      setTimeout(() => setInstalledId(null), 6000);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function removeSkill(id: string) {
    setError(null);
    setBusy(true);
    try {
      const updated = await invoke<AgentDetail>("agent_skill_uninstall", {
        name: detail.agent_name,
        skillId: id,
      });
      onSaved(updated);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="tab-form">
      <button
        className="btn btn--sm btn--primary"
        onClick={installSkill}
        disabled={busy}
        style={{ alignSelf: "flex-start" }}
      >
        {t("detail.installSkill")}
      </button>
      <p className="field-muted" style={{ fontSize: 12 }}>
        {t("detail.skillInstallFormatHint")}
      </p>
      {installedId && (
        <p className="save-ok" style={{ fontSize: 12 }}>
          {t("detail.skillInstalledOk", { id: installedId })}
        </p>
      )}
      {error && <p className="save-error">{error}</p>}

      {!hasInstalled && !hasLegacy && (
        <div className="tab-empty">
          <p>{t("detail.noSkills")}</p>
          <p className="field-muted" style={{ fontSize: 12 }}>
            {t("detail.skillInstallHint")}
          </p>
        </div>
      )}

      {hasInstalled && (
        <>
          <label className="field-label" style={{ marginTop: 12 }}>
            {t("detail.installedSkills", { count: detail.installed_skills.length })}
          </label>
          <ul className="item-list">
            {detail.installed_skills.map((s) => (
              <li key={s.name} className="item-card">
                <button
                  className="item-card-remove"
                  title={t("detail.remove")}
                  aria-label={t("detail.remove")}
                  disabled={busy}
                  onClick={() => removeSkill(s.name)}
                >
                  ×
                </button>
                <div className="item-card-name">{s.name}</div>
                {s.version && (
                  <span className="badge-sm">{s.version}</span>
                )}
                {s.description && (
                  <div className="item-card-desc">{s.description}</div>
                )}
                {s.category && (
                  <span className="field-muted" style={{ fontSize: 11 }}>{s.category}</span>
                )}
              </li>
            ))}
          </ul>
        </>
      )}

      {hasLegacy && (
        <>
          <label className="field-label" style={{ marginTop: hasInstalled ? 16 : 12 }}>
            {t("detail.legacySkillPaths", { count: detail.skills.length })}
          </label>
          <ul className="item-list">
            {detail.skills.map((s) => (
              <li key={s.path} className="item-card">
                <button
                  className="item-card-remove"
                  title={t("detail.remove")}
                  aria-label={t("detail.remove")}
                  disabled={busy}
                  onClick={() => removeSkill(s.path)}
                >
                  ×
                </button>
                <div>
                  <code style={{ fontSize: 11 }}>{s.path}</code>
                  <span className={s.loadable ? "badge-loadable" : "badge-dead"}>
                    {s.loadable ? t("detail.skillLoadable") : t("detail.skillDead")}
                  </span>
                </div>
                {!s.loadable && (
                  <div className="item-card-desc field-muted" style={{ fontSize: 11 }}>
                    {t("detail.skillDeadHint")}
                  </div>
                )}
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}

// ─── MCP Tab ──────────────────────────────────────────────────────────────

function McpTab({
  detail,
  onSaved,
}: {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
}) {
  const { t } = useT();
  const [showForm, setShowForm] = useState(false);
  const [serverId, setServerId] = useState("");
  const [command, setCommand] = useState("");
  const [args, setArgs] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [justAdded, setJustAdded] = useState(false);

  async function browseCommand() {
    const picked = await open({ multiple: false }).catch(() => null);
    if (typeof picked === "string" && picked) setCommand(picked);
  }

  async function addServer() {
    setError(null);
    setBusy(true);
    try {
      const updated = await invoke<AgentDetail>("agent_mcp_add", {
        name: detail.agent_name,
        serverId: serverId.trim(),
        command: command.trim(),
        args: args.trim() ? args.trim().split(/\s+/) : [],
      });
      onSaved(updated);
      setShowForm(false);
      setServerId("");
      setCommand("");
      setArgs("");
      setJustAdded(true);
      setTimeout(() => setJustAdded(false), 4000);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function removeServer(id: string) {
    setError(null);
    setBusy(true);
    try {
      const updated = await invoke<AgentDetail>("agent_mcp_remove", {
        name: detail.agent_name,
        serverId: id,
      });
      onSaved(updated);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="tab-form">
      {!showForm && (
        <button
          className="btn btn--sm btn--primary"
          onClick={() => setShowForm(true)}
          style={{ alignSelf: "flex-start" }}
        >
          {t("detail.addMcp")}
        </button>
      )}
      {justAdded && (
        <p className="field-muted" style={{ fontSize: 12 }}>
          {t("detail.mcpAddedHint")}
        </p>
      )}

      {showForm && (
        <div className="mcp-add-form">
          <label className="field-label">{t("detail.mcpId")}</label>
          <input
            className="input"
            value={serverId}
            placeholder="media"
            onChange={(e) => setServerId(e.target.value)}
          />
          <label className="field-label">{t("detail.mcpCommand")}</label>
          <div className="mcp-command-row">
            <input
              className="input"
              value={command}
              placeholder="/usr/local/bin/my-mcp-server"
              onChange={(e) => setCommand(e.target.value)}
            />
            <button className="toolbar-btn" onClick={browseCommand} disabled={busy}>
              {t("detail.browse")}
            </button>
          </div>
          <label className="field-label">{t("detail.mcpArgs")}</label>
          <input
            className="input"
            value={args}
            placeholder="--flag value"
            onChange={(e) => setArgs(e.target.value)}
          />
          <div className="mcp-form-actions">
            <button
              className="btn btn--sm btn--primary"
              onClick={addServer}
              disabled={busy || !serverId.trim() || !command.trim()}
            >
              {busy ? t("detail.saving") : t("detail.add")}
            </button>
            <button
              className="btn btn--sm btn--secondary"
              onClick={() => {
                setShowForm(false);
                setError(null);
              }}
              disabled={busy}
            >
              {t("detail.cancel")}
            </button>
          </div>
        </div>
      )}
      {error && <p className="save-error">{error}</p>}

      {detail.mcp_servers.length === 0 ? (
        <div className="tab-empty">
          <p>{t("detail.noMcp")}</p>
          <p className="field-muted" style={{ fontSize: 12 }}>
            {t("detail.mcpAddHint")}
          </p>
        </div>
      ) : (
        <>
          <label className="field-label" style={{ marginTop: 12 }}>
            {t("detail.mcpServersCount", { count: detail.mcp_servers.length })}
          </label>
          <ul className="item-list">
            {detail.mcp_servers.map((m) => (
              <li key={m.name} className="item-card">
                <button
                  className="item-card-remove"
                  title={t("detail.remove")}
                  aria-label={t("detail.remove")}
                  disabled={busy}
                  onClick={() => removeServer(m.name)}
                >
                  ×
                </button>
                <div className="item-card-name">{m.name}</div>
                <code className="item-card-code">{m.command}</code>
                {m.args.length > 0 && (
                  <div className="item-card-args">
                    {m.args.map((a, i) => (
                      <span key={i} className="badge-sm">{a}</span>
                    ))}
                  </div>
                )}
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}

// ─── Permissions Tab ──────────────────────────────────────────────────────

function PermissionsTab({ detail }: { detail: AgentDetail }) {
  const { t } = useT();
  return (
    <div className="tab-form">
      <label className="field-label">{t("detail.capabilities")}</label>
      {detail.capabilities.length === 0 ? (
        <p className="field-muted" style={{ fontSize: 12 }}>{t("detail.noCaps")}</p>
      ) : (
        <div className="badge-row">
          {detail.capabilities.map((c) => (
            <span key={c} className="cap-tag"><span className="cap-dot" />{c}</span>
          ))}
        </div>
      )}

      <label className="field-label" style={{ marginTop: 16 }}>{t("detail.mcpServers")}</label>
      <p className="field-muted" style={{ fontSize: 12 }}>
        {detail.mcp_servers.length === 0
          ? t("detail.noMcp")
          : t("detail.mcpSummary", { count: detail.mcp_servers.length })}
      </p>

      <label className="field-label" style={{ marginTop: 16 }}>{t("detail.skills")}</label>
      <p className="field-muted" style={{ fontSize: 12 }}>
        {detail.installed_skills.length === 0 && detail.skills.length === 0
          ? t("detail.noSkills")
          : t("detail.skillsSummary", {
              installed: detail.installed_skills.length,
              legacy: detail.skills.length,
            })}
      </p>

      <p className="field-muted" style={{ marginTop: 24, fontSize: 11, fontStyle: "italic" }}>
        {t("detail.permissionsHint")}
      </p>
    </div>
  );
}
