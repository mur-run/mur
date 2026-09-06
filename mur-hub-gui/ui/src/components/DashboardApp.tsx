import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useAgents } from "../context/AgentContext";
import type { AgentRuntimeStatus, NudgeStatus } from "../types";
import { WizardModal } from "./wizard/WizardModal";
import { PresetImportModal } from "./PresetImportModal";
import { MuragentImportModal } from "./MuragentImportModal";
import { SettingsModal } from "./SettingsModal";
import { ModelSetupWizard } from "./ModelSetupWizard";
import { ModelPickerModal } from "./ModelPickerModal";
import { ModelsPage } from "./library/ModelsPage";
import { InstallInboxModal } from "./InstallInboxModal";
import { Inspector, hasInspector, type InspectorSelection } from "./shell/Inspector";
import { HomePage } from "./home/HomePage";
import { useChannels } from "./home/useChannels";
import { needsYouCounts } from "./home/needsYouCounts";
import { useInbox } from "./home/useInbox";
import { inboxBadge, visibleInboxItems } from "./home/inbox";
import type { InboxItem } from "./home/inbox";
import { ChatsPage } from "./chats/ChatsPage";
import { FleetView } from "./fleet/FleetView";
import { useT } from "../i18n";
import type { TranslationKey } from "../i18n/types";
import { Shell } from "./shell/Shell";
import { NAV_ITEMS, isPageId, type PageId } from "./shell/nav";
import { isEditingTarget, isOpenInWindowShortcut, openDetailWindow } from "./detail/window/openInWindow";
import type { FleetSummary } from "./fleet/types";
import { CommandPalette, isPaletteShortcut } from "./shell/CommandPalette";
import type { PaletteItem } from "./shell/palette";
import { AgentsPage } from "./agents/AgentsPage";
import { SkillsPage } from "./library/SkillsPage";
import { McpPage } from "./library/McpPage";
import { PluginsPage } from "./library/PluginsPage";
import { WorkflowsPage } from "./library/WorkflowsPage";

// ─── PlaceholderPage ─────────────────────────────────────────────────────────

// Library / home pages not yet built in the redesign. Shows the page label, a
// "coming in this redesign" line, and — where a corresponding modal already
// exists — a button that opens it (currently only the models page → ModelLibrary).
function PlaceholderPage({ id, onOpen }: { id: PageId; onOpen?: () => void }) {
  const { t } = useT();
  return (
    <div className="shell-placeholder">
      <div className="shell-placeholder__icon" aria-hidden="true">
        <svg viewBox="0 0 24 24" width="40" height="40" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
          <path d="M12 3v3M12 18v3M3 12h3M18 12h3M5.6 5.6l2.1 2.1M16.3 16.3l2.1 2.1M18.4 5.6l-2.1 2.1M7.7 16.3l-2.1 2.1" />
        </svg>
      </div>
      <h3>{t(`nav.${id}` as TranslationKey)}</h3>
      <p>{t("placeholder.body")}</p>
      {onOpen && (
        <button className="btn btn--primary" onClick={onOpen}>
          {t("placeholder.open")}
        </button>
      )}
    </div>
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
  // Active shell page. Home is the default mission-control surface.
  const [page, setPage] = useState<PageId>("home");
  // Per-page selection that drives the contextual right-pane Inspector. The
  // agents-page selection lives in AgentContext (selectedAgent); these cover
  // the chats / fleets / library pages.
  const [chatAgent, setChatAgent] = useState<{ name: string; displayName?: string } | null>(null);
  // Stable callbacks so the pages' report-up effects don't loop.
  const onChatActive = useCallback((name: string | null, displayName?: string) => {
    setChatAgent(name ? { name, displayName } : null);
  }, []);
  // Unified inbox — owned here so the sidebar + Dock badges stay in sync with
  // what HomePage renders.
  const { items: inboxItems, refresh: refreshInbox } = useInbox();
  // Session-local dismissal (e.g. "keep" on a blocked-upgrade card). Lifted
  // here — the single source of truth — so the sidebar/Dock badge and the
  // NeedsYou list are always computed from the same filtered array.
  const [dismissedInbox, setDismissedInbox] = useState<Set<string>>(new Set());
  const visibleInbox = visibleInboxItems(inboxItems, dismissedInbox);
  // Per-agent "needs you" counts for the Agents list (spec §6.3), and the
  // channel summaries the agent Overview reads (same source as Home).
  const needsYou = needsYouCounts(visibleInbox);
  const { channels } = useChannels();
  function dismissInboxItem(it: InboxItem) {
    setDismissedInbox((prev) => {
      const next = new Set(prev);
      next.add(`${it.kind}:${it.id}`);
      return next;
    });
  }
  const [paletteOpen, setPaletteOpen] = useState(false);
  // Agents → Chat: which agent the Chats page should open on.
  const [chatInitial, setChatInitial] = useState<string | null>(null);
  const openChatWith = useCallback((name: string) => {
    setChatInitial(name);
    setPage("chats");
  }, []);
  const [fleetRequest, setFleetRequest] = useState<string | null>(null);
  // The Fleets page reports its selection up for ⌘↩ and the palette (spec 2(b) §7).
  const [selectedFleet, setSelectedFleet] = useState<string | null>(null);
  const onFleetSelect = useCallback((name: string | null) => setSelectedFleet(name), []);
  const [paletteFleets, setPaletteFleets] = useState<FleetSummary[]>([]);
  // Handed to FleetView so a palette jump to the same fleet twice still fires.
  const clearFleetRequest = useCallback(() => setFleetRequest(null), []);
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

  // Mirror the unified-inbox count to the macOS Dock / taskbar badge.
  const badgeCount = inboxBadge(visibleInbox);
  useEffect(() => {
    getCurrentWindow()
      .setBadgeCount(badgeCount > 0 ? badgeCount : undefined)
      .catch(() => {});
  }, [badgeCount]);

  useEffect(() => {
    const unSelect = listen<string>("select-agent", (e) => {
      setSelected(e.payload);
      // Selection drives the agents-page inspector, so surface that page.
      setPage("agents");
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

  // "Show in Hub" from a fleet detail window, and page jumps from any window.
  useEffect(() => {
    const unFleet = listen<string>("select-fleet", (e) => {
      setFleetRequest(e.payload);
      setPage("fleets");
    });
    const unPage = listen<string>("open-page", (e) => {
      if (isPageId(e.payload)) setPage(e.payload);
    });
    return () => {
      void unFleet.then((fn) => fn());
      void unPage.then((fn) => fn());
    };
  }, []);

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

  // Check nudge dismissal on mount; offer the upgrade only while the concierge
  // is still on its stock brain — once the user has picked a model there is
  // nothing to nudge about.
  useEffect(() => {
    invoke<NudgeStatus>("nudge_status")
      .then((s) => {
        setNudgeDismissed(s.dismissed);
        if (!s.dismissed && s.stock_brain) setShowUpgradeNudge(true);
      })
      .catch(() => {});
  }, []);

  // Surface a CLI/Hub version skew on mount (None unless `mur` lags the Hub).
  useEffect(() => {
    invoke<{ cli: string; hub: string; upgrade_hint: string } | null>("cli_version_skew")
      .then((s) => setCliSkew(s))
      .catch(() => {});
  }, []);



  // ⌘K opens the command palette; ⌘R refreshes (spec §6.6).
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (isPaletteShortcut(e)) {
        e.preventDefault();
        setPaletteOpen(true);
      } else if (isOpenInWindowShortcut(e)) {
        if (isEditingTarget(document.activeElement)) return;
        if (page === "agents" && selectedAgent) {
          e.preventDefault();
          const a = agents.find((x) => x.name === selectedAgent);
          void openDetailWindow("agent", selectedAgent, a?.display_name ?? selectedAgent);
        } else if (page === "fleets" && selectedFleet) {
          e.preventDefault();
          const f = paletteFleets.find((x) => x.name === selectedFleet);
          void openDetailWindow("fleet", selectedFleet, f?.display_name ?? selectedFleet);
        }
      } else if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "r") {
        e.preventDefault();
        invoke("list_agents").catch(console.error);
        refreshInbox();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [refreshInbox, page, selectedAgent, selectedFleet, agents, paletteFleets]);

  // Fleets are not held in app state; fetch them when the palette opens.
  useEffect(() => {
    if (!paletteOpen) return;
    invoke<FleetSummary[]>("fleet_list")
      .then(setPaletteFleets)
      .catch(() => setPaletteFleets([]));
  }, [paletteOpen]);

  // Esc deselects the current page's inspector target (which auto-hides the
  // column). Ignored while a modal/input is focused so it doesn't fight them.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key !== "Escape") return;
      const el = document.activeElement;
      if (el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.tagName === "SELECT")) return;
      if (el && el.getAttribute("role") === "listbox") return; // SourceList owns its Esc
      setSelected(null);
      setChatAgent(null);
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [setSelected]);

  // Build the contextual inspector for the current page + selection.
  const inspectorSelection: InspectorSelection = {
    agent: selectedAgent,
    chatAgent: chatAgent?.name ?? null,
    chatDisplayName: chatAgent?.displayName,
  };
  const inspectorNode = hasInspector(page, inspectorSelection) ? (
    <Inspector
      page={page}
      selection={inspectorSelection}
      onClose={() => {
        setSelected(null);
        setChatAgent(null);
      }}
    />
  ) : undefined;

  const selectedRuntime = selectedAgent ? runtimeMap.get(selectedAgent)?.state.state : undefined;
  const paletteItems: PaletteItem[] = [
    ...NAV_ITEMS.map((n) => ({ id: `page:${n.id}`, kind: "page" as const, label: t(n.labelKey), run: () => setPage(n.id) })),
    { id: "action:newChat", kind: "action", label: t("palette.action.newChat"), run: () => setPage("chats") },
    { id: "action:newAgent", kind: "action", label: t("palette.action.newAgent"), run: () => setWizardOpen(true) },
    { id: "action:settings", kind: "action", label: t("palette.action.settings"), run: () => setSettingsOpen(true) },
    {
      id: "action:refresh",
      kind: "action",
      label: t("palette.action.refresh"),
      run: () => {
        invoke("list_agents").catch(console.error);
        refreshInbox();
      },
    },
    ...(selectedAgent
      ? [
          selectedRuntime === "running"
            ? {
                id: "action:stop",
                kind: "action" as const,
                label: t("palette.action.stop", { name: selectedAgent }),
                run: () => {
                  invoke("stop_agent", { name: selectedAgent }).catch(console.error);
                },
              }
            : {
                id: "action:start",
                kind: "action" as const,
                label: t("palette.action.start", { name: selectedAgent }),
                run: () => {
                  invoke("start_agent", { name: selectedAgent }).catch(console.error);
                },
              },
          {
            id: "action:openAgentInWindow",
            kind: "action" as const,
            label: t("palette.action.openInWindow", { name: selectedAgent }),
            run: () => {
              const a = agents.find((x) => x.name === selectedAgent);
              void openDetailWindow("agent", selectedAgent, a?.display_name ?? selectedAgent);
            },
          },
        ]
      : []),
    ...(selectedFleet
      ? [{
          id: "action:openFleetInWindow",
          kind: "action" as const,
          label: t("palette.action.openInWindow", { name: selectedFleet }),
          run: () => {
            const f = paletteFleets.find((x) => x.name === selectedFleet);
            void openDetailWindow("fleet", selectedFleet, f?.display_name ?? selectedFleet);
          },
        }]
      : []),
    ...agents.map((a) => ({
      id: `agent:${a.name}`,
      kind: "agent" as const,
      label: a.display_name,
      hint: a.role ?? undefined,
      run: () => {
        setPage("agents");
        setSelected(a.name);
      },
    })),
    ...paletteFleets.map((f) => ({
      id: `fleet:${f.name}`,
      kind: "fleet" as const,
      label: f.display_name,
      run: () => {
        setPage("fleets");
        setFleetRequest(f.name);
      },
    })),
  ];

  // Banners render inside the content column (Shell `banners` slot), so
  // they never push the sidebar down (spec §3.3).
  const banners = (
    <>
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
              setModelPickerOpen(true);
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
    </>
  );

  return (
    <div className="dashboard-root">
      <div className="dashboard-main dashboard">
        <Shell
          page={page}
          onNavigate={(id) => setPage(id)}
          badge={badgeCount}
          inspector={inspectorNode}
          banners={banners}
          onSettings={() => setSettingsOpen(true)}
          onSearch={() => setPaletteOpen(true)}
        >
          {page === "home" ? (
            <HomePage
              agents={agents}
              runtimeStatuses={runtimeStatuses}
              items={visibleInbox}
              onRefresh={refreshInbox}
              onDismiss={dismissInboxItem}
              onNavigate={(id) => setPage(id)}
              onCreateAgent={() => setWizardOpen(true)}
            />
          ) : page === "chats" ? (
            <ChatsPage agents={agents} initialAgent={chatInitial} onActiveChange={onChatActive} />
          ) : page === "fleets" ? (
            <FleetView onSelect={onFleetSelect} requestedName={fleetRequest} onRequestHandled={clearFleetRequest} />
          ) : page === "agents" ? (
            <AgentsPage
              agents={agents}
              runtimeMap={runtimeMap}
              channels={channels}
              needsYou={needsYou}
              selectedAgent={selectedAgent}
              onNewAgent={() => setWizardOpen(true)}
              onOpenChat={openChatWith}
              onOpenHome={() => setPage("home")}
            />
          ) : page === "models" ? (
            <ModelsPage />
          ) : page === "skills" ? (
            <SkillsPage />
          ) : page === "mcp" ? (
            <McpPage />
          ) : page === "workflows" ? (
            <WorkflowsPage />
          ) : page === "plugins" ? (
            <PluginsPage />
          ) : (
            <PlaceholderPage id={page} />
          )}
        </Shell>
      </div>

      <WizardModal
        isOpen={wizardOpen}
        onClose={(name) => {
          setWizardOpen(false);
          if (name) invoke("list_agents").catch(() => {});
        }}
        onImport={() => setMuragentImportOpen(true)}
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
        dismissible
      />
      <InstallInboxModal />
      <CommandPalette open={paletteOpen} items={paletteItems} onClose={() => setPaletteOpen(false)} />
    </div>
  );
}

