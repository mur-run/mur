//! Agent inspector — contextual right-pane column shown in the Shell's
//! inspector slot when an agent is selected. Restyled from the former
//! full-page detail panel; content (status header + the
//! persona/style/behavior/skills/MCP/permissions/inbox/mobile/memory/plugins
//! tabs) is unchanged. Tab bodies live in ./tabs/*.
//!
//! ⌘⌥I toggles the whole column (Shell). `onClose` (the × button, or Esc
//! from the caller) clears the selection which auto-hides the column.

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import type { AgentEntry, AgentRuntimeStatus } from "../../types";
import { ALL_DETAIL_TABS, type AgentDetail, type DetailTab } from "../../types";
import { useAgents } from "../../context/AgentContext";
import { CompanionInbox } from "../CompanionInbox";
import { ModelCombobox } from "../ModelCombobox";
import { ModelLibrary } from "../ModelLibrary";
import { FallbackChainEditor } from "../settings/FallbackChainEditor";
import { sanitizeChain } from "../settings/modelSwitch";
import type { ModelOption } from "../modelPicker";
import { MobileTab } from "../MobileTab";
import { MemoryTab } from "../MemoryTab";
import { useT } from "../../i18n";
import type { TranslationKey } from "../../i18n/types";
import { CATEGORY_COLORS, TAB_ICONS, avatarInitials, avatarPreset, familyOf } from "../../utils";
import { StatusPill, statusOf } from "../shell/Status";
import { PetFace } from "../PetFace";
import { PersonaTab } from "./tabs/PersonaTab";
import { StyleTab } from "./tabs/StyleTab";
import { BehaviorTab } from "./tabs/BehaviorTab";
import { SkillsTab } from "./tabs/SkillsTab";
import { McpTab } from "./tabs/McpTab";
import { PluginsTab } from "./tabs/PluginsTab";
import { PermissionsTab } from "./tabs/PermissionsTab";
import { ScheduleTab } from "./tabs/ScheduleTab";

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
  plugins: "detail.plugins",
  schedule: "detail.schedule",
};

interface Props {
  agentName: string;
  agents: AgentEntry[];
  /** Live supervisor runtime for this agent — same source the list uses, so
   *  the header status matches the card (was derived from the lock-based
   *  AgentEntry.status, which could disagree). */
  runtime?: AgentRuntimeStatus;
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

export function AgentInspector({ agentName, agents, runtime, onClose }: Props) {
  const { t } = useT();
  const { desiredDetailTab, setDesiredDetailTab } = useAgents();
  const [detail, setDetail] = useState<AgentDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [activeTab, setActiveTab] = useState<DetailTab>("persona");
  const [libraryOpen, setLibraryOpen] = useState(false);
  const [modelOptions, setModelOptions] = useState<ModelOption[]>([]);
  const [agentChain, setAgentChain] = useState<string[]>([]);
  const [chainErr, setChainErr] = useState<string | null>(null);
  // Per-agent Smart override. null = follow the global setting.
  const [agentSmart, setAgentSmart] = useState<boolean | null>(null);

  useEffect(() => {
    setError(null);
    setActiveTab("persona");
    invoke<AgentDetail>("get_agent_detail", { name: agentName })
      .then(setDetail)
      .catch((e) => setError(String(e)));
  }, [agentName]);

  // Per-agent fallback-chain override (empty = inherits the global chain set
  // in Settings → Models). Independent of `detail` — has its own Tauri pair.
  useEffect(() => {
    setChainErr(null);
    invoke<string[]>("agent_get_fallback", { name: agentName })
      .then(setAgentChain)
      .catch(() => setAgentChain([]));
  }, [agentName]);

  useEffect(() => {
    invoke<boolean | null>("agent_get_smart", { name: agentName })
      .then(setAgentSmart)
      .catch(() => setAgentSmart(null));
  }, [agentName]);

  useEffect(() => {
    invoke<ModelOption[]>("list_models")
      .then(setModelOptions)
      .catch(() => setModelOptions([]));
  }, []);

  function saveAgentChain(next: string[]) {
    const refs = sanitizeChain(next);
    invoke<string[]>("agent_set_fallback", { name: agentName, refs })
      .then((saved) => {
        setAgentChain(saved);
        setChainErr(null);
      })
      .catch((e) => setChainErr(String(e)));
  }

  function saveAgentSmart(state: string) {
    invoke<boolean | null>("agent_set_smart", { name: agentName, state })
      .then(setAgentSmart)
      .catch((e) => setChainErr(String(e)));
  }

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
  // Status from the live runtime (same source as the agent list), so the header
  // pill and the card never disagree.
  const rtState = runtime?.state.state;
  const isRunning = rtState === "running" || rtState === "restarting";

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
    const preset = entry ? avatarPreset(entry) : null;
    const avatarColor = CATEGORY_COLORS[entry?.category ?? "custom"] ?? "#64748B";
    return (
      <div className="detail-panel__header">
        <div className="detail-panel__top">
          {preset ? (
            // The agent's pet face — same avatar the grid card shows, so the
            // detail header isn't a bare initials square.
            <div className="detail-panel__avatar detail-panel__avatar--pet">
              <PetFace
                presetId={preset}
                family={familyOf(preset)}
                expression="idle"
                size={48}
              />
            </div>
          ) : (
            <div
              className="detail-panel__avatar"
              style={{ background: avatarColor, color: "#fff", fontSize: "18px", fontWeight: 700 }}
            >
              {avatarInitials(name)}
            </div>
          )}
          <div className="detail-panel__ident">
            <div className="detail-panel__name">{name}</div>
            <StatusPill kind={statusOf(runtime?.state)} />
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
      <aside className="detail-panel detail-panel--inspector">
        <Header name={displayName} />
        <div className="detail-panel__body">
          <p className="detail-error">{t("detail.loadFailed", { error })}</p>
        </div>
      </aside>
    );
  }

  if (!detail) {
    return (
      <aside className="detail-panel detail-panel--inspector">
        <Header name={displayName} />
        <div className="detail-panel__body">
          <p className="detail-loading">{t("detail.loading")}</p>
        </div>
      </aside>
    );
  }

  return (
    <aside className="detail-panel detail-panel--inspector">
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
            <div className="tab-form" style={{ marginBottom: 18 }}>
              <label className="field-label">{t("detail.fallbackChain")}</label>
              <FallbackChainEditor
                chain={agentChain}
                options={modelOptions}
                primaryRef={detail.model_ref}
                onChange={saveAgentChain}
                emptyHintKey="detail.fallbackChainInherits"
              />
              {chainErr && (
                <p className="settings-hint slot-error">
                  {t("detail.fallbackChainError", { error: chainErr })}
                </p>
              )}
              <label className="field-label" htmlFor="agent-smart">
                {t("detail.smartRouting")}
              </label>
              <select
                id="agent-smart"
                value={agentSmart === null ? "follow" : agentSmart ? "on" : "off"}
                onChange={(e) => saveAgentSmart(e.target.value)}
              >
                <option value="follow">{t("detail.smartFollow")}</option>
                <option value="on">{t("detail.smartOn")}</option>
                <option value="off">{t("detail.smartOff")}</option>
              </select>
              <p className="settings-hint">{t("detail.smartHint")}</p>
            </div>
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
        {activeTab === "plugins" && <PluginsTab detail={detail} onSaved={handleSaved} />}
        {activeTab === "schedule" && <ScheduleTab agentName={agentName} />}
      </div>
    </aside>
  );
}
