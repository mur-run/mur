import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { AgentProvider, useAgents } from "../../../context/AgentContext";
import { useT } from "../../../i18n";
import { visibleInboxItems } from "../../home/inbox";
import { needsYouCounts } from "../../home/needsYouCounts";
import { useChannels } from "../../home/useChannels";
import { useInbox } from "../../home/useInbox";
import { DirtyProvider, useDirtyGuard } from "../../shell/dirty";
import { isMac } from "../../shell/platform";
import { AgentDetail } from "../agent/AgentDetail";
import { FleetHost } from "../fleet/FleetHost";
import { parseDetailRoute, type DetailRoute } from "./detailRoute";

/** Dismissals are session state in DashboardApp; a fresh window has none. */
const NOTHING_DISMISSED: ReadonlySet<string> = new Set();

/** The `#/detail/<kind>/<name>` root (spec 2(b) §4): the dashboard's
 *  providers, a drag bar with "Show in Hub", and one detail filling the window. */
export function DetailWindow() {
  const route = parseDetailRoute(window.location.hash);
  return (
    <AgentProvider>
      <DirtyProvider>
        <DetailWindowInner route={route} />
      </DirtyProvider>
    </AgentProvider>
  );
}

function DetailWindowInner({ route }: { route: DetailRoute | null }) {
  const { t } = useT();
  const { confirmLeave } = useDirtyGuard();

  // Closing with unsaved edits asks first (spec §8); the same prompt the
  // dashboard shows when switching selection.
  useEffect(() => {
    const un = getCurrentWindow().onCloseRequested(async (e) => {
      if (!(await confirmLeave(t("detail.discardBody"), t("detail.discardTitle")))) e.preventDefault();
    });
    return () => { void un.then((f) => f()); };
  }, [confirmLeave, t]);

  function showInHub() {
    const args = !route ? {} : route.kind === "agent" ? { agentName: route.name } : { fleetName: route.name };
    invoke("open_dashboard", args).catch(console.error);
  }

  return (
    <div className={`detail-window${isMac() ? " detail-window--inset" : ""}`}>
      <div className="detail-window__bar" data-tauri-drag-region>
        <button type="button" className="btn btn--secondary" onClick={showInHub}>
          {t("detailWindow.showInHub")}
        </button>
      </div>
      {!route ? (
        <Missing text={t("detailWindow.badRoute")} />
      ) : route.kind === "agent" ? (
        <AgentBody name={route.name} />
      ) : (
        <FleetHost
          name={route.name}
          missing={<Missing text={t("detailWindow.missingFleet")} />}
          onDeleted={() => void getCurrentWindow().close()}
        />
      )}
    </div>
  );
}

function Missing({ text }: { text: string }) {
  const { t } = useT();
  return (
    <div className="detail-window__missing">
      <p>{text}</p>
      <button type="button" className="btn btn--secondary" onClick={() => void getCurrentWindow().close()}>
        {t("detailWindow.close")}
      </button>
    </div>
  );
}

function AgentBody({ name }: { name: string }) {
  const { t } = useT();
  const { agents, runtimeStatuses } = useAgents();
  const { channels } = useChannels();
  const { items } = useInbox();
  const needsYou = needsYouCounts(visibleInboxItems(items, NOTHING_DISMISSED));
  const entry = agents.find((a) => a.name === name);
  // Before the list arrives `entry` is undefined and the header shows the
  // raw name; once it has arrived, an absent entry means the agent is gone.
  if (agents.length > 0 && !entry) return <Missing text={t("detailWindow.missingAgent")} />;
  return (
    <AgentDetail
      agentName={name}
      entry={entry}
      runtime={runtimeStatuses.find((s) => s.name === name)}
      channels={channels}
      needsYou={needsYou[name] ?? 0}
      onOpenChat={(agentName) => {
        invoke("open_chat_window", { agentName }).catch(console.error);
      }}
      onOpenHome={() => {
        invoke("open_dashboard", { page: "home" }).catch(console.error);
      }}
    />
  );
}
