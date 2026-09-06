import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import type { AgentEntry, AgentRuntimeStatus, AgentDetail as AgentDetailData } from "../../../types";
import type { ChannelSummary } from "../../../work/types";
import type { ModelOption } from "../../modelPicker";
import { useAgents } from "../../../context/AgentContext";
import { useT } from "../../../i18n";
import { CATEGORY_COLORS, avatarInitials, avatarPreset, familyOf } from "../../../utils";
import { PetFace } from "../../PetFace";
import { sanitizeChain } from "../../settings/modelSwitch";
import { DetailPage } from "../../shell/DetailPage";
import { OverflowMenu } from "../../shell/OverflowMenu";
import { statusOf } from "../../shell/Status";
import { AGENT_TABS, AGENT_TAB_LABEL_KEY, detailGroupOf, type AgentTabId } from "../../shell/detailTabs";
import { useDirtyGuard } from "../../shell/dirty";
import { MemoryTab } from "../../MemoryTab";
import { ScheduleTab } from "../../inspector/tabs/ScheduleTab";
import { IdentityTab } from "./IdentityTab";
import { CapabilitiesTab } from "./CapabilitiesTab";
import { ChannelsTab } from "./ChannelsTab";
import { OverviewTab } from "./OverviewTab";

export interface AgentDetailProps {
  agentName: string;
  entry: AgentEntry | undefined;
  runtime: AgentRuntimeStatus | undefined;
  channels: ChannelSummary[];
  needsYou: number;
  onOpenChat: (name: string) => void;
  onOpenHome: () => void;
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

/** The agent's full-width detail pane (spec §4.3): AgentInspector's data and
 *  handlers, rendered through DetailPage with the six grouped tabs. */
export function AgentDetail({ agentName, entry, runtime, channels, needsYou, onOpenChat, onOpenHome }: AgentDetailProps) {
  const { t } = useT();
  const { desiredDetailTab, setDesiredDetailTab } = useAgents();
  const { confirmLeave } = useDirtyGuard();
  const [detail, setDetail] = useState<AgentDetailData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tab, setTab] = useState<AgentTabId>("overview");
  const [libraryOpen, setLibraryOpen] = useState(false);
  const [modelOptions, setModelOptions] = useState<ModelOption[]>([]);
  const [agentChain, setAgentChain] = useState<string[]>([]);
  const [chainErr, setChainErr] = useState<string | null>(null);
  // Per-agent Smart override. null = follow the global setting.
  const [agentSmart, setAgentSmart] = useState<boolean | null>(null);

  useEffect(() => {
    setError(null);
    setTab("overview");
    invoke<AgentDetailData>("get_agent_detail", { name: agentName })
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

  // Deep link: the legacy tab id (e.g. 🎨 on a card → "style") resolves to
  // the new tab plus an in-tab anchor; consumed once.
  useEffect(() => {
    if (!desiredDetailTab) return;
    const g = detailGroupOf(desiredDetailTab);
    setTab(g.tab);
    setDesiredDetailTab(null);
    if (g.anchor) {
      requestAnimationFrame(() => document.getElementById(`agent-${g.anchor}`)?.scrollIntoView({ block: "start" }));
    }
  }, [desiredDetailTab, setDesiredDetailTab]);

  async function changeTab(next: AgentTabId) {
    if (next === tab) return;
    if (await confirmLeave(t("detail.discardBody"), t("detail.discardTitle"))) setTab(next);
  }

  // Status from the live runtime (same source as the list), so the header
  // pill and the row never disagree.
  const rt = runtime?.state.state;
  const isRunning = rt === "running" || rt === "restarting";
  const displayName = detail?.display_name ?? entry?.display_name ?? agentName;
  const preset = entry ? avatarPreset(entry) : null;
  const avatar = preset ? (
    <PetFace presetId={preset} family={familyOf(preset)} expression="idle" size={48} />
  ) : (
    <span className="detail-page__initials" style={{ background: CATEGORY_COLORS[entry?.category ?? "custom"] }}>
      {avatarInitials(displayName)}
    </span>
  );

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

  const actions = (
    <>
      <button type="button" className="btn btn--primary" onClick={() => onOpenChat(agentName)}>
        {t("action.chat")}
      </button>
      {isRunning ? (
        <button type="button" className="btn btn--secondary" onClick={() => handleStop(agentName)}>
          {t("action.stop")}
        </button>
      ) : (
        <button type="button" className="btn btn--secondary" onClick={() => handleRun(agentName)}>
          {t("action.run")}
        </button>
      )}
      <OverflowMenu
        label={t("action.more")}
        items={[
          { id: "export", label: t("action.export"), onSelect: () => { void handleExport(agentName); } },
          {
            id: "chatWindow",
            label: t("action.openChatWindow"),
            onSelect: () => {
              invoke("open_chat_window", { agentName }).catch(console.error);
            },
          },
        ]}
      />
    </>
  );

  const meta = (
    <>
      {entry?.role && <span>{entry.role}</span>}
      {entry?.role && <span className="sep">·</span>}
      <span className="mono">{entry?.model_id ?? "—"}</span>
    </>
  );

  return (
    <DetailPage
      avatar={avatar}
      title={displayName}
      status={statusOf(runtime?.state)}
      meta={meta}
      actions={actions}
      tabs={AGENT_TABS.map((id) => ({ id, label: t(AGENT_TAB_LABEL_KEY[id]) }))}
      activeTab={tab}
      onTab={(id) => {
        void changeTab(id);
      }}
      banners={error ? <p className="detail-error">{t("detail.loadFailed", { error })}</p> : undefined}
    >
      {tab === "overview" && (
        <OverviewTab
          detail={detail}
          channels={channels}
          agentName={agentName}
          needsYou={needsYou}
          onGoTo={(id) => {
            void changeTab(id);
          }}
          onOpenChat={() => onOpenChat(agentName)}
          onOpenHome={onOpenHome}
        />
      )}
      {detail && tab === "identity" && (
        <IdentityTab
          detail={detail}
          onSaved={setDetail}
          modelOptions={modelOptions}
          agentChain={agentChain}
          chainErr={chainErr}
          onChain={saveAgentChain}
          agentSmart={agentSmart}
          onSmart={saveAgentSmart}
          libraryOpen={libraryOpen}
          setLibraryOpen={setLibraryOpen}
        />
      )}
      {detail && tab === "capabilities" && <CapabilitiesTab detail={detail} onSaved={setDetail} />}
      {tab === "memory" && <MemoryTab agentName={agentName} />}
      {tab === "automation" && <ScheduleTab agentName={agentName} />}
      {tab === "channels" && <ChannelsTab agentName={agentName} />}
      {!detail && !error && tab !== "overview" && <p className="detail-loading">{t("detail.loading")}</p>}
    </DetailPage>
  );
}
