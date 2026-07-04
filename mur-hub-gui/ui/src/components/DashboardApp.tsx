import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { currentMonitor, getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { save } from "@tauri-apps/plugin-dialog";
import { useAgents } from "../context/AgentContext";
import type { AgentEntry, AgentRuntimeStatus } from "../types";
import { WizardModal } from "./wizard/WizardModal";
import { PresetImportModal } from "./PresetImportModal";
import { MuragentImportModal } from "./MuragentImportModal";
import { SettingsModal } from "./SettingsModal";
import { ModelSetupWizard } from "./ModelSetupWizard";
import { ModelPickerModal } from "./ModelPickerModal";
import { useUnreadCount } from "./CompanionInbox";
import { DetailPanel } from "./DetailPanel";
import { ConversationsView } from "./ConversationsView";
import { WorkView } from "./work/WorkView";
import { ChatsView } from "./ChatsView";
import { FleetView } from "./fleet/FleetView";
import { useConversations } from "../conversation/ConversationContext";
import { Mascot } from "./Mascot";
import type { MascotMood } from "./Mascot";
import { PetFace } from "./PetFace";
import { useT } from "../i18n";
import { CATEGORY_COLORS, avatarInitials, avatarPreset, familyOf, runtimePill, timeGreetingKey } from "../utils";

// ─── Shared helpers ────────────────────────────────────────────────────────

function showToast(msg: string, durationMs = 2000) {
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = msg;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), durationMs);
}


// Monochrome (currentColor) action glyphs — WKWebView ignores font-variant-emoji,
// so emoji codepoints render in color; inline SVG is the only reliable mono path.
function Ico({ filled, children }: { filled?: boolean; children: ReactNode }) {
  return (
    <svg
      viewBox="0 0 24 24"
      width="15"
      height="15"
      fill={filled ? "currentColor" : "none"}
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {children}
    </svg>
  );
}

// ─── GridCard ──────────────────────────────────────────────────────────────

interface GridCardProps {
  agent: AgentEntry;
  runtime: AgentRuntimeStatus | undefined;
  isSelected: boolean;
}

export function GridCard({ agent, runtime, isSelected }: GridCardProps) {
  const { t } = useT();
  const { setSelected } = useAgents();
  const unread = useUnreadCount(agent.name);
  const color = CATEGORY_COLORS[agent.category] ?? "#6B7280";
  const pill = runtimePill(runtime?.state);
  const isRunning = runtime?.state.state === "running";
  const isBusy = runtime?.state.state === "restarting";

  // Drag-to-spawn state
  const holdTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [dragging, setDragging] = useState(false);
  const [ghostPos, setGhostPos] = useState({ x: 0, y: 0 });
  const cursorOutsideRef = useRef(false);
  const mouseDownPosRef = useRef({ x: 0, y: 0 });

  async function handleRun() {
    await invoke("start_agent", { name: agent.name }).catch((e) =>
      showToast(t("dashboard.startFailed", { error: String(e) })),
    );
  }
  async function handleStop() {
    await invoke("stop_agent", { name: agent.name }).catch((e) =>
      showToast(t("dashboard.stopFailed", { error: String(e) })),
    );
  }
  async function handleShare() {
    const outPath = await save({
      defaultPath: `${agent.name}.muragent`,
      filters: [{ name: "MUR Agent", extensions: ["muragent"] }],
    });
    if (!outPath) return;
    invoke<string>("export_muragent_file", { name: agent.name, outPath })
      .then(() => showToast(`Exported ${agent.name}.muragent`))
      .catch((e) => showToast(`Export failed: ${e}`, 6000));
  }

  function startHold(e: React.MouseEvent) {
    if (e.button !== 0) return;
    mouseDownPosRef.current = { x: e.clientX, y: e.clientY };
    holdTimer.current = setTimeout(() => {
      holdTimer.current = null;
      setDragging(true);
      setGhostPos({ x: e.screenX, y: e.screenY });
      cursorOutsideRef.current = false;
    }, 300);
  }

  function cancelHold() {
    if (holdTimer.current) {
      clearTimeout(holdTimer.current);
      holdTimer.current = null;
    }
  }

  // A natural press-and-drag leaves the card before the hold timer fires; treat
  // that as entering the drag instead of cancelling, or the pet can never spawn.
  function leaveCard(e: React.MouseEvent) {
    if (holdTimer.current && (e.buttons & 1) !== 0) {
      cancelHold();
      setDragging(true);
      setGhostPos({ x: e.screenX, y: e.screenY });
      cursorOutsideRef.current = false;
      return;
    }
    cancelHold();
  }

  useEffect(() => {
    if (!dragging) return;

    function onMove(e: MouseEvent) {
      setGhostPos({ x: e.screenX, y: e.screenY });
    }
    function onLeave() { cursorOutsideRef.current = true; }
    function onEnter() { cursorOutsideRef.current = false; }
    function onUp(e: MouseEvent) {
      setDragging(false);
      // Treat the release as "dropped on the desktop" if it lands outside the Hub
      // window. Decide from the release coordinates vs the window bounds rather than
      // relying on a document `mouseleave`, which does NOT fire during a button-held
      // drag out of the window (macOS captures mouse events to the origin window), so
      // the pet would otherwise never spawn.
      const outsideByBounds =
        e.screenX < window.screenX ||
        e.screenX > window.screenX + window.outerWidth ||
        e.screenY < window.screenY ||
        e.screenY > window.screenY + window.outerHeight;
      if (cursorOutsideRef.current || outsideByBounds) {
        invoke("pet_spawn_at", {
          agentName: agent.name,
          screenX: e.screenX,
          screenY: e.screenY,
        }).catch((err) => showToast(`Pet: ${err}`));
      }
    }
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    document.addEventListener("mouseleave", onLeave);
    document.addEventListener("mouseenter", onEnter);
    return () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      document.removeEventListener("mouseleave", onLeave);
      document.removeEventListener("mouseenter", onEnter);
    };
  }, [dragging, agent.name]);

  return (
    <>
      <div
        className={`grid-card${isSelected ? " grid-card--selected" : ""}${dragging ? " grid-card--dragging" : ""}`}
        style={{ ["--cat" as string]: color }}
        data-agent={agent.name}
        onMouseDown={startHold}
        onMouseUp={cancelHold}
        onMouseLeave={leaveCard}
        onClick={() => setSelected(isSelected ? null : agent.name)}
      >
        <div className="grid-card__head">
          <div className="grid-card__avatar grid-card__avatar--pet" title={avatarPreset(agent)}>
            <PetFace
              presetId={avatarPreset(agent)}
              family={familyOf(avatarPreset(agent))}
              expression="idle"
              size={44}
              animate={false}
            />
            {unread > 0 && (
              <span className="unread-badge">{unread > 99 ? "99+" : unread}</span>
            )}
          </div>
          <div>
            <p className="grid-card__name">{agent.display_name}</p>
            {agent.role && <span className="role-chip">{agent.role}</span>}
            <p className="grid-card__cat">{t(`category.${agent.category}` as Parameters<typeof t>[0])}</p>
          </div>
        </div>
        <div className="grid-card__actions">
          <button
            disabled={isRunning || isBusy}
            onClick={(e) => { e.stopPropagation(); handleRun(); }}
            title={t("dashboard.run")}
            aria-label={t("dashboard.run")}
          >
            <Ico filled><polygon points="6 4 20 12 6 20 6 4" /></Ico>
          </button>
          <button
            disabled={!isRunning && !isBusy}
            onClick={(e) => { e.stopPropagation(); handleStop(); }}
            title={t("dashboard.stop")}
            aria-label={t("dashboard.stop")}
          >
            <Ico filled><rect x="6" y="6" width="12" height="12" rx="1.5" /></Ico>
          </button>
          <button onClick={(e) => { e.stopPropagation(); handleShare(); }} title={t("dashboard.share")} aria-label={t("dashboard.share")}>
            <Ico>
              <path d="M4 12v7a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-7" />
              <polyline points="16 7 12 3 8 7" />
              <line x1="12" y1="3" x2="12" y2="15" />
            </Ico>
          </button>
          <button
            onClick={(e) => {
              e.stopPropagation();
              invoke("open_chat_window", { agentName: agent.name }).catch(console.error);
            }}
            title={t("detail.chat")}
            aria-label={t("detail.chat")}
          >
            <Ico>
              <path d="M21 11.5a8.38 8.38 0 0 1-.9 3.8 8.5 8.5 0 0 1-7.6 4.7 8.38 8.38 0 0 1-3.8-.9L3 21l1.9-5.7a8.38 8.38 0 0 1-.9-3.8 8.5 8.5 0 0 1 4.7-7.6 8.38 8.38 0 0 1 3.8-.9h.5a8.48 8.48 0 0 1 8 8v.5z" />
            </Ico>
          </button>

        </div>
        <div className="grid-card__foot">
          <span className={pill.cls}>
            <span className="pill__dot" />
            {t(pill.key)}
          </span>
          <button
            className="grid-card__settings"
            onClick={(e) => { e.stopPropagation(); setSelected(agent.name); }}
            title={t("dashboard.detail")}
            aria-label={t("dashboard.detail")}
          >
            <Ico>
              <path d="M2 12s3-7 10-7 10 7 10 7-3 7-10 7-10-7-10-7Z" />
              <circle cx="12" cy="12" r="3" />
            </Ico>
          </button>
        </div>
      </div>

      {dragging && (
        <div
          className="drag-ghost"
          style={{
            position: "fixed",
            left: ghostPos.x - window.screenX - 40,
            top: ghostPos.y - window.screenY - 40,
            pointerEvents: "none",
          }}
        >
          <div className="drag-ghost-avatar" style={{ background: color }}>
            {avatarInitials(agent.display_name)}
          </div>
          <span className="drag-ghost-label">{agent.display_name}</span>
        </div>
      )}
    </>
  );
}

// ─── ListRow ───────────────────────────────────────────────────────────────

interface ListRowProps {
  agent: AgentEntry;
  runtime: AgentRuntimeStatus | undefined;
  isSelected: boolean;
}

export function ListRow({ agent, runtime, isSelected }: ListRowProps) {
  const { t } = useT();
  const { setSelected } = useAgents();
  const color = CATEGORY_COLORS[agent.category] ?? "#6B7280";
  const pill = runtimePill(runtime?.state);
  const model =
    agent.model_id.length > 24 ? agent.model_id.slice(0, 24) + "…" : agent.model_id;
  return (
    <div
      className={`list-row${isSelected ? " list-row--selected" : ""}`}
      style={{ ["--cat" as string]: color }}
      data-agent={agent.name}
      onClick={() => setSelected(isSelected ? null : agent.name)}
    >
      <div className="list-row__main">
        <div className="list-avatar" style={{ background: color }}>
          {avatarInitials(agent.display_name)}
        </div>
        <span className="list-name">{agent.display_name}</span>
        {agent.role && <span className="role-chip">{agent.role}</span>}
      </div>
      <span className="list-category">{t(`category.${agent.category}` as Parameters<typeof t>[0])}</span>
      <span className="list-model" title={agent.model_id}>
        {model}
      </span>
      <span className={pill.cls}>
        <span className="pill__dot" />
        {t(pill.key)}
      </span>
      <button
        className="list-row__settings"
        onClick={(e) => { e.stopPropagation(); setSelected(isSelected ? null : agent.name); }}
        title={t("dashboard.settings")}
        aria-label={t("dashboard.settings")}
      >
        <Ico>
          <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
          <circle cx="12" cy="12" r="3" />
        </Ico>
      </button>
    </div>
  );
}

// ─── Sidebar ───────────────────────────────────────────────────────────────

/** Sentinel for the "no role assigned" sidebar bucket. */
export const NO_ROLE = "__none__";

interface SidebarProps {
  activeRole: string | null;
  agents: AgentEntry[];
  onSelect: (role: string | null) => void;
}

/**
 * Left rail filters by ROLE (was persona category, which was useless — nearly
 * every agent is "custom"). Lists each distinct role + a "no role" bucket.
 */
export function Sidebar({ activeRole, agents, onSelect }: SidebarProps) {
  const { t } = useT();
  const counts: Record<string, number> = {};
  let noRole = 0;
  for (const a of agents) {
    const r = a.role?.trim();
    if (r) counts[r] = (counts[r] ?? 0) + 1;
    else noRole++;
  }
  const roles = Object.keys(counts).sort((x, y) => x.localeCompare(y));

  return (
    <nav className="sidebar">
      <button
        className={`sidebar-item${activeRole === null ? " sidebar-item--active" : ""}`}
        onClick={() => onSelect(null)}
      >
        {t("dashboard.all")} <span className="badge">{agents.length}</span>
      </button>
      {roles.map((role) => (
        <button
          key={role}
          className={`sidebar-item${activeRole === role ? " sidebar-item--active" : ""}`}
          onClick={() => onSelect(role)}
        >
          <span className="sidebar-item__icon">🎭</span>
          {role} <span className="badge">{counts[role]}</span>
        </button>
      ))}
      {noRole > 0 && (
        <button
          className={`sidebar-item${activeRole === NO_ROLE ? " sidebar-item--active" : ""}`}
          onClick={() => onSelect(NO_ROLE)}
        >
          <span className="sidebar-item__icon">∅</span>
          {t("dashboard.noRole")} <span className="badge">{noRole}</span>
        </button>
      )}
    </nav>
  );
}

// ─── DashboardApp ──────────────────────────────────────────────────────────

// Subset of @tauri-apps/plugin-updater's DownloadEvent we read for progress.
type UpdaterEvent =
  | { event: "Started"; data?: { contentLength?: number } }
  | { event: "Progress"; data?: { chunkLength?: number } }
  | { event: "Finished"; data?: unknown };

export function DashboardApp() {
  const { t } = useT();
  const { agents, runtimeStatuses, selectedAgent, setSelected } = useAgents();
  const { open: openConvs } = useConversations();
  const [activeRole, setActiveRole] = useState<string | null>(null);
  const [viewMode, setViewMode] = useState<"grid" | "list">("grid");
  const [surface, setSurface] = useState<"agents" | "chats" | "work" | "fleet">("agents");
  const [query, setQuery] = useState("");
  const [wizardOpen, setWizardOpen] = useState(false);
  const [presetImportOpen, setPresetImportOpen] = useState(false);
  const [muragentImportOpen, setMuragentImportOpen] = useState(false);
  const [muragentImportPath, setMuragentImportPath] = useState<string | undefined>(undefined);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [showModelWizard, setShowModelWizard] = useState(false);
  const [modelPickerOpen, setModelPickerOpen] = useState(false);
  const [showAppsBanner, setShowAppsBanner] = useState(false);
  const [showUpgradeNudge, setShowUpgradeNudge] = useState(false);
  const [nudgeDismissed, setNudgeDismissed] = useState(false);
  // Passive nudge when the PATH `mur` CLI lags this Hub. The Hub never
  // auto-upgrades it (that would clobber a brew binary) — just surface it.
  const [cliSkew, setCliSkew] = useState<{ cli: string; hub: string; upgrade_hint: string } | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  // App auto-update. Detect on mount, but never download silently: a Hub update
  // is a large payload, so surface a banner and let the user choose to install
  // (with progress + error feedback) instead of an invisible background fetch.
  const updateRef = useRef<{
    version: string;
    downloadAndInstall: (cb?: (e: UpdaterEvent) => void) => Promise<void>;
  } | null>(null);
  const [appUpdate, setAppUpdate] = useState<{ version: string } | null>(null);
  const [updateProgress, setUpdateProgress] = useState<number | null>(null);
  const [updateError, setUpdateError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    // Check on mount, and again whenever the window regains focus — a Hub left
    // open for days would otherwise never notice a release published meanwhile.
    // ponytail: focus-only, no timer; add an interval if it must notice while focused for hours.
    const runCheck = () =>
      import("@tauri-apps/plugin-updater")
        .then(({ check }) => check())
        .then((u) => {
          if (u && !cancelled) {
            updateRef.current = u;
            setAppUpdate({ version: u.version });
          }
        })
        .catch((e) => console.warn("Update check failed:", e));
    runCheck();
    window.addEventListener("focus", runCheck);
    return () => {
      cancelled = true;
      window.removeEventListener("focus", runCheck);
    };
  }, []);

  async function installUpdate() {
    const u = updateRef.current;
    if (!u) return;
    setUpdateError(null);
    setUpdateProgress(0);
    try {
      let downloaded = 0;
      let total = 0;
      await u.downloadAndInstall((e) => {
        if (e.event === "Started") total = e.data?.contentLength ?? 0;
        else if (e.event === "Progress") {
          downloaded += e.data?.chunkLength ?? 0;
          setUpdateProgress(total ? Math.round((downloaded / total) * 100) : null);
        } else if (e.event === "Finished") setUpdateProgress(100);
      });
      const { relaunch } = await import("@tauri-apps/plugin-process");
      await relaunch();
    } catch (e) {
      setUpdateProgress(null);
      setUpdateError(String(e));
    }
  }

  // Build a lookup map for runtime statuses.
  const runtimeMap = new Map<string, AgentRuntimeStatus>(
    runtimeStatuses.map((s) => [s.name, s]),
  );

  // Flock stats for the hero: count agents whose runtime is actively running.
  const runningCount = agents.filter(
    (a) => runtimeMap.get(a.name)?.state.state === "running",
  ).length;
  const idleCount = agents.length - runningCount;

  const mascotMood: MascotMood =
    agents.length === 0
      ? "excited"
      : runningCount === agents.length
        ? "happy"
        : runningCount === 0
          ? "worried"
          : "idle";

  const mascotBubble =
    mascotMood === "excited"
      ? t("mascot.bubble.excited")
      : mascotMood === "happy"
        ? t("mascot.bubble.happy")
        : mascotMood === "worried"
          ? t("mascot.bubble.worried")
          : t("mascot.bubble.idle", { running: runningCount, idle: idleCount });

  useEffect(() => {
    const unSelect = listen<string>("select-agent", (e) => {
      setSelected(e.payload);
      setTimeout(() => {
        document
          .querySelector(`[data-agent="${e.payload}"]`)
          ?.scrollIntoView({ behavior: "smooth", block: "center" });
      }, 50);
    });
    return () => {
      unSelect.then((fn) => fn());
    };
  }, [setSelected]);

  // Pet "Chat" / file-drop → open the dedicated chat window for that agent.
  useEffect(() => {
    const unsub = listen<{ agent: string; draft?: string | null }>("pet-open-chat", (e) => {
      const { agent } = e.payload;
      invoke("open_chat_window", { agentName: agent }).catch(console.error);
    });
    return () => { unsub.then((fn) => fn()); };
  }, []);

  // Listen for .muragent file open events from OS file association / deep-link
  useEffect(() => {
    const unlisten = listen<string>("open-muragent-file", (e) => {
      setMuragentImportPath(e.payload);
      setMuragentImportOpen(true);
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Listen for "open-wizard" event emitted by the popover's New Agent button
  useEffect(() => {
    const unlisten = listen("open-wizard", () => setWizardOpen(true));
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // Listen for "open-settings" emitted by the app menu's Settings… item (Cmd+,).
  useEffect(() => {
    const unlisten = listen("open-settings", () => setSettingsOpen(true));
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // Listen for "need-model" emitted by backend on first run when no model is configured.
  useEffect(() => {
    const unlisten = listen("need-model", () => setModelPickerOpen(true));
    return () => { unlisten.then((fn) => fn()); };
  }, []);

  // Auto-resize window when the detail panel or conversation surface
  // opens/closes, so the dashboard keeps a usable width instead of being
  // squeezed by the side panels. Clamped to the monitor so the window
  // never grows past the visible screen.
  const hasConvs = openConvs.length > 0;
  useEffect(() => {
    (async () => {
      const win = getCurrentWindow();
      const monitor = await currentMonitor().catch(() => null);
      const scale = monitor?.scaleFactor ?? 1;
      const availW = monitor ? monitor.size.width / scale - 16 : 1440;
      const availH = monitor ? monitor.size.height / scale - 60 : 800;
      const desiredW = 960 + (selectedAgent ? 240 : 0) + (hasConvs ? 480 : 0);
      const desiredH = selectedAgent || hasConvs ? 720 : 620;
      const width = Math.min(desiredW, availW);
      const height = Math.min(desiredH, availH);
      win.setSize(new LogicalSize(width, height)).catch(console.error);
      const minW = Math.min(
        720 + (selectedAgent ? 180 : 0) + (hasConvs ? 360 : 0),
        availW,
      );
      win
        .setMinSize(new LogicalSize(minW, Math.min(480, availH)))
        .catch(console.error);
    })().catch(console.error);
  }, [selectedAgent, hasConvs]);

  // First-launch check: show banner if not running from /Applications
  useEffect(() => {
    invoke<{ is_first_launch: boolean; in_applications: boolean }>(
      "check_first_launch"
    ).then((status) => {
      if (!status.in_applications) {
        setShowAppsBanner(true);
      }
      if (status.is_first_launch) {
        invoke("mark_first_launch_done").catch(() => {});
        invoke<{ needs_setup: boolean }>("model_setup_status")
          .then((s) => { if (s.needs_setup) setShowModelWizard(true); })
          .catch(() => {});
      }
    }).catch(() => {});
  }, []);

  // Check nudge dismissal on mount; show upgrade banner when not dismissed and a model is available.
  useEffect(() => {
    invoke<[boolean, string | null]>("nudge_status")
      .then(([dismissed, model]) => {
        setNudgeDismissed(dismissed);
        if (!dismissed && model) setShowUpgradeNudge(true);
      })
      .catch(() => {});
  }, []);

  // Surface a CLI/Hub version skew on mount (None unless `mur` lags the Hub).
  useEffect(() => {
    invoke<{ cli: string; hub: string; upgrade_hint: string } | null>("cli_version_skew")
      .then((s) => setCliSkew(s))
      .catch(() => {});
  }, []);



  // ⌘K focus search.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        searchRef.current?.focus();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const q = query.toLowerCase();
  const visible = agents.filter(
    (a) =>
      (activeRole === null ||
        (activeRole === NO_ROLE ? !a.role?.trim() : a.role?.trim() === activeRole)) &&
      (!q || a.name.toLowerCase().includes(q) || a.display_name.toLowerCase().includes(q)),
  );

  return (
    <div className="dashboard-root">
      <div className="dashboard-main dashboard">
        {showAppsBanner && (
          <div className="onboarding-banner">
            <span>
              {t("dashboard.moveToAppsBody", {
                folder: t("dashboard.applicationsFolder"),
              })
                .split(t("dashboard.applicationsFolder"))
                .flatMap((part, i) =>
                  i === 0
                    ? [part]
                    : [
                        <strong key={i}>{t("dashboard.applicationsFolder")}</strong>,
                        part,
                      ],
                )}
            </span>
            <button
              className="toolbar-btn"
              onClick={() => setShowAppsBanner(false)}
              title={t("dashboard.dismiss")}
            >
              ✕
            </button>
          </div>
        )}
        {showUpgradeNudge && !nudgeDismissed && (
          <div className="upgrade-nudge-banner">
            <span>{t("dashboard.nudgePrompt")}</span>
            <div className="upgrade-nudge-actions">
              <button
                className="toolbar-btn"
                onClick={() => {
                  invoke("nudge_dismiss").catch(() => {});
                  setNudgeDismissed(true);
                }}
              >
                {t("dashboard.nudgeDecline")}
              </button>
              <button
                className="toolbar-btn toolbar-btn--primary"
                onClick={() => {
                  setShowUpgradeNudge(false);
                  setWizardOpen(true);
                }}
              >
                {t("dashboard.nudgeAccept")}
              </button>
            </div>
          </div>
        )}
        {appUpdate && (
          <div className="upgrade-nudge-banner">
            <span>
              {updateError
                ? t("dashboard.updateError", { error: updateError })
                : updateProgress !== null
                  ? t("dashboard.updateDownloading", {
                      version: appUpdate.version,
                      pct: updateProgress,
                    })
                  : t("dashboard.updateAvailable", { version: appUpdate.version })}
            </span>
            {updateProgress === null && (
              <div className="upgrade-nudge-actions">
                <button className="toolbar-btn" onClick={() => setAppUpdate(null)}>
                  {t("dashboard.updateLater")}
                </button>
                <button
                  className="toolbar-btn toolbar-btn--primary"
                  onClick={installUpdate}
                >
                  {updateError
                    ? t("dashboard.updateRetry")
                    : t("dashboard.updateInstall")}
                </button>
              </div>
            )}
          </div>
        )}
        {cliSkew && (
          <div className="upgrade-nudge-banner">
            <span>
              {t("dashboard.cliSkew", { cli: cliSkew.cli, hub: cliSkew.hub, hint: cliSkew.upgrade_hint })}
            </span>
            <button
              className="toolbar-btn"
              onClick={() => setCliSkew(null)}
              title={t("dashboard.dismiss")}
            >
              ✕
            </button>
          </div>
        )}

        <div className="dashboard__bar">
          <span className="dashboard__brand">MUR</span>
          <nav className="surface-toggle dashboard__bar-nav">
            <button
              className={surface === "agents" ? "is-active" : ""}
              onClick={() => setSurface("agents")}
            >
              {t("work.toggle.agents")}
            </button>
            <button
              className={surface === "chats" ? "is-active" : ""}
              onClick={() => setSurface("chats")}
            >
              {t("work.toggle.chats")}
            </button>
            <button
              className={surface === "work" ? "is-active" : ""}
              onClick={() => setSurface("work")}
            >
              {t("work.toggle.work")}
            </button>
            <button
              className={surface === "fleet" ? "is-active" : ""}
              onClick={() => setSurface("fleet")}
            >
              {t("fleet.tab")}
            </button>
          </nav>
          <label className="field dashboard__bar-search">
            <input
              ref={searchRef}
              type="search"
              placeholder={t("dashboard.search")}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          <div className="dashboard__bar-tools">
            <button
              className="toolbar-btn toolbar-btn--primary"
              onClick={() => setWizardOpen(true)}
            >
              + {t("app.newAgent")}
            </button>
            <div className="view-toggle">
              <button
                className={viewMode === "grid" ? "is-active" : ""}
                onClick={() => setViewMode("grid")}
                title={t("view.grid")}
                aria-label={t("view.grid")}
              >
                ⊞
              </button>
              <button
                className={viewMode === "list" ? "is-active" : ""}
                onClick={() => setViewMode("list")}
                title={t("view.list")}
                aria-label={t("view.list")}
              >
                ☰
              </button>
            </div>
            <button
              className="toolbar-btn toolbar-btn--icon"
              title={t("app.refresh")}
              aria-label={t("app.refresh")}
              onClick={() =>
                invoke<AgentEntry[]>("list_agents")
                  .then(() => {}) // state updated via event
                  .catch(console.error)
              }
            >
              ↺
            </button>
            <button
              className="toolbar-btn toolbar-btn--icon"
              title={t("app.settings")}
              aria-label={t("app.settings")}
              onClick={() => setSettingsOpen(true)}
            >
              ⚙
            </button>
          </div>
        </div>

        {surface === "fleet" ? (
          <FleetView query={query} />
        ) : surface === "work" ? (
          <WorkView agents={agents} query={query} />
        ) : surface === "chats" ? (
          <ChatsView agents={agents} query={query} />
        ) : (
          <div className="agents-view">
            <Sidebar activeRole={activeRole} agents={agents} onSelect={setActiveRole} />
            <div className="agents-view__content">
            <div className="dashboard__hero">
              <Mascot floating mood={mascotMood} bubble={mascotBubble} />
              <div>
                <h3>{t(timeGreetingKey())}</h3>
                <p>
                  {t("dashboard.flockStatus", {
                    running: runningCount,
                    idle: idleCount,
                  })}
                </p>
              </div>
              <div className="dashboard__stats">
                <div className="stat">
                  <div className="stat__n stat__n--run">{runningCount}</div>
                  <div className="stat__l">{t("dashboard.stat.running")}</div>
                </div>
                <div className="stat">
                  <div className="stat__n">{idleCount}</div>
                  <div className="stat__l">{t("dashboard.stat.idle")}</div>
                </div>
              </div>
            </div>

            <div className="dashboard-content">
              {visible.length === 0 ? (
                <div className="empty-state">
                  <Mascot floating size={96} mood="excited" bubble={t("mascot.bubble.excited")} />
                  <h3>{t("dashboard.empty.title")}</h3>
                  <p>{t("dashboard.empty.body")}</p>
                  <button className="btn btn--primary" onClick={() => setWizardOpen(true)}>
                    {t("dashboard.empty.cta")}
                  </button>
                </div>
              ) : viewMode === "grid" ? (
                <div className="agent-grid">
                  {visible.map((a) => (
                    <GridCard
                      key={a.name}
                      agent={a}
                      runtime={runtimeMap.get(a.name)}
                      isSelected={selectedAgent === a.name}
                    />
                  ))}
                </div>
              ) : (
                <div className="agent-list">
                  <div className="agent-list__head">
                    <span>{t("dashboard.col.agent")}</span>
                    <span>{t("dashboard.col.category")}</span>
                    <span>{t("dashboard.col.model")}</span>
                    <span>{t("dashboard.col.status")}</span>
                    <span />
                  </div>
                  {visible.map((a) => (
                    <ListRow
                      key={a.name}
                      agent={a}
                      runtime={runtimeMap.get(a.name)}
                      isSelected={selectedAgent === a.name}
                    />
                  ))}
                </div>
              )}
            </div>
            </div>{/* agents-view__content */}
          </div>
        )}
      </div>

      {/* Conversation rail — slides in when conversations are open */}
      {openConvs.length > 0 && <ConversationsView />}

      {/* Detail panel — slides in when an agent is selected */}
      {selectedAgent && (
        <DetailPanel
          agentName={selectedAgent}
          agents={agents}
          runtime={runtimeMap.get(selectedAgent)}
          onClose={() => setSelected(null)}
        />
      )}

      <WizardModal
        isOpen={wizardOpen}
        onClose={(name) => {
          setWizardOpen(false);
          if (name) invoke("list_agents").catch(() => {});
        }}
      />
      <PresetImportModal
        isOpen={presetImportOpen}
        onClose={() => setPresetImportOpen(false)}
      />
      <MuragentImportModal
        isOpen={muragentImportOpen}
        initialPath={muragentImportPath}
        onClose={() => {
          setMuragentImportOpen(false);
          setMuragentImportPath(undefined);
          invoke("list_agents").catch(() => {});
        }}
      />
      <ModelSetupWizard
        open={showModelWizard}
        onClose={() => setShowModelWizard(false)}
        onCustomize={() => {
          setShowModelWizard(false);
          setSettingsOpen(true);
        }}
      />
      <SettingsModal
        isOpen={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        onImportAgent={() => setMuragentImportOpen(true)}
        onImportPreset={() => setPresetImportOpen(true)}
      />
      <ModelPickerModal
        isOpen={modelPickerOpen}
        onClose={() => setModelPickerOpen(false)}
      />
    </div>
  );
}
