import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useT } from "../../../i18n";
import type { AgentEntry } from "../../../types";
import type { FleetSummary, FleetDetail as Detail, JobRow, LabelView } from "../../fleet/types";
import { Ico } from "../../agents/GridCard";
import { DetailPage } from "../../shell/DetailPage";
import { fleetStatusOf } from "../../shell/Status";
import { FLEET_TABS, FLEET_TAB_LABEL_KEY, type FleetTabId } from "../../shell/detailTabs";
import { FleetHeader, fleetMeta } from "./FleetHeader";
import { FleetOverview } from "./FleetOverview";
import { FleetMembers } from "./FleetMembers";
import { FleetJobs } from "./FleetJobs";
import { FleetSettings } from "./FleetSettings";
import { showToast } from "./fleetActions";

/** The fleet glyph: list rows (28px) and the detail avatar (48px). */
export const FLEET_GLYPH = (
  <>
    <path d="M12 4l9 4.5-9 4.5-9-4.5z" />
    <path d="M3 13l9 4.5 9-4.5" />
  </>
);

export interface FleetDetailPaneProps {
  name: string;
  /** Status + labels, from `fleet_list`. */
  summary: FleetSummary;
  labels: LabelView[];
  agentMap: Map<string, AgentEntry>;
  /** The host reloads its list / labels; the pane reloads detail + jobs itself. */
  onRefresh: () => void;
  /** After a successful delete: the host clears its selection or closes the window. */
  onDeleted: () => void;
}

/** One fleet's detail page (spec 2(b) §5): owns `fleet_detail` + `fleet_jobs`,
 *  reloads on `fleet:run_done` for this fleet, and renders the four tabs.
 *  Hosts key it by `name`, so a selection change remounts it. */
export function FleetDetailPane({ name, summary, labels, agentMap, onRefresh, onDeleted }: FleetDetailPaneProps) {
  const { t } = useT();
  const [detail, setDetail] = useState<Detail | null>(null);
  const [jobs, setJobs] = useState<JobRow[]>([]);
  const [tab, setTab] = useState<FleetTabId>("overview");

  const load = useCallback(async () => {
    try {
      const [d, j] = await Promise.all([
        invoke<Detail>("fleet_detail", { name }),
        invoke<JobRow[]>("fleet_jobs", { name, all: false }),
      ]);
      setDetail(d);
      setJobs(j);
    } catch (err) {
      showToast(String(err), 4000);
    }
  }, [name]);

  useEffect(() => {
    void load();
  }, [load]);

  // A finished run for this fleet refreshes its jobs; the host toasts and
  // reloads the list (FleetView keeps that so it fires without a selection).
  useEffect(() => {
    const unlisten = listen<{ name: string; ok: boolean }>("fleet:run_done", (event) => {
      if (event.payload.name === name) void load();
    });
    return () => { void unlisten.then((fn) => fn()); };
  }, [name, load]);

  // Refetch when this window is focused (spec 2(b) §6). Fleet forms never
  // mark dirty, so no guard is needed here.
  useEffect(() => {
    const un = getCurrentWindow().onFocusChanged(({ payload: focused }) => {
      if (focused) void load();
    });
    return () => { void un.then((f) => f()); };
  }, [load]);

  function refresh() {
    onRefresh();
    void load();
  }

  if (!detail) return null;

  return (
    <DetailPage
      avatar={<span className="fleet-avatar fleet-avatar--lg" aria-hidden="true"><Ico>{FLEET_GLYPH}</Ico></span>}
      title={detail.display_name}
      status={fleetStatusOf(summary)}
      meta={fleetMeta(detail, t)}
      actions={<FleetHeader detail={detail} onRefresh={refresh} onDelete={onDeleted} />}
      tabs={FLEET_TABS.map((id) => ({ id, label: t(FLEET_TAB_LABEL_KEY[id]) }))}
      activeTab={tab}
      onTab={setTab}
    >
      {tab === "overview" && <FleetOverview detail={detail} jobs={jobs} agentMap={agentMap} onGoTo={setTab} />}
      {tab === "members" && (
        <FleetMembers detail={detail} agentMap={agentMap} labels={labels} fleetLabels={summary.labels} onRefresh={refresh} />
      )}
      {tab === "jobs" && <FleetJobs detail={detail} jobs={jobs} onRefresh={refresh} />}
      {tab === "settings" && <FleetSettings detail={detail} onRefresh={refresh} onDelete={onDeleted} />}
    </DetailPage>
  );
}
