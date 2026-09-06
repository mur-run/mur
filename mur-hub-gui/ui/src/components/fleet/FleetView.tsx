import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useT } from "../../i18n";
import type { AgentEntry } from "../../types";
import type { FleetSummary, FleetDetail as Detail, JobRow, LabelView } from "./types";
import { UNGROUPED } from "./fleetLabels";
import { FleetCreateModal } from "./FleetCreateModal";
import { Ico } from "../agents/GridCard";
import { SourceList } from "../shell/SourceList";
import type { SourceFacet, SourceRowData } from "../shell/sourceListModel";
import { ListDivider } from "../shell/ListDivider";
import {
  LIST_WIDTH_DEFAULT, LIST_WIDTH_MAX, LIST_WIDTH_MIN, useResizableColumn,
} from "../shell/useResizableColumn";
import { DetailPage } from "../shell/DetailPage";
import { fleetStatusOf } from "../shell/Status";
import { FLEET_TABS, FLEET_TAB_LABEL_KEY, type FleetTabId } from "../shell/detailTabs";
import { listModeFor } from "../shell/breakpoints";
import { useWindowWidth } from "../shell/useWindowWidth";
import { readKey, writeKey } from "../shell/persist";
import { FleetHeader, fleetMeta } from "../detail/fleet/FleetHeader";
import { FleetOverview } from "../detail/fleet/FleetOverview";
import { FleetMembers } from "../detail/fleet/FleetMembers";
import { FleetJobs } from "../detail/fleet/FleetJobs";
import { FleetSettings } from "../detail/fleet/FleetSettings";
import { showToast } from "../detail/fleet/fleetActions";

const LABEL_FILTER_KEY = "mur.fleet.labelFilter";
export const LAST_SELECTED_FLEET_KEY = "mur.fleets.lastSelected";
export const FLEETS_LIST_WIDTH_KEY = "mur.fleets.listWidth";

/** The label filter persists as a list (older builds stored several ids);
 *  the chips are single-select now, so only the first survives. */
function loadLabelFilter(): string | null {
  try {
    const raw = localStorage.getItem(LABEL_FILTER_KEY);
    const parsed = raw ? JSON.parse(raw) : null;
    return Array.isArray(parsed) && typeof parsed[0] === "string" ? parsed[0] : null;
  } catch {
    return null;
  }
}

const FLEET_GLYPH = (
  <>
    <path d="M12 4l9 4.5-9 4.5-9-4.5z" />
    <path d="M3 13l9 4.5 9-4.5" />
  </>
);

export function FleetView({ onSelect, requestedName, onRequestHandled }: {
  onSelect?: (name: string | null) => void;
  /** The command palette can ask for a fleet by name (spec §6.6). */
  requestedName?: string | null;
  /** Called once the request is applied, so the same fleet can be requested again. */
  onRequestHandled?: () => void;
}) {
  const { t } = useT();
  const [fleets, setFleets] = useState<FleetSummary[]>([]);
  const [selectedName, setSelectedName] = useState<string | null>(null);
  const [detail, setDetail] = useState<Detail | null>(null);
  const [jobs, setJobs] = useState<JobRow[]>([]);
  const [showCreate, setShowCreate] = useState(false);
  const [agentMap, setAgentMap] = useState<Map<string, AgentEntry>>(new Map());
  const [labels, setLabels] = useState<LabelView[]>([]);
  const [activeLabel, setActiveLabel] = useState<string | null>(loadLabelFilter);
  const [filter, setFilter] = useState("");
  const [tab, setTab] = useState<FleetTabId>("overview");
  const [listShown, setListShown] = useState(false);
  const selectedRef = useRef<string | null>(null);
  // One-shot: the first fleet_list restores the last selection (or picks the
  // first fleet); later reloads and a cleared selection never re-fill it.
  const restoredRef = useRef(false);
  const column = useResizableColumn(FLEETS_LIST_WIDTH_KEY, LIST_WIDTH_DEFAULT, LIST_WIDTH_MIN, LIST_WIDTH_MAX);
  const listMode = listModeFor(useWindowWidth());

  // Persist the chip selection so the list comes back the way it was left.
  useEffect(() => {
    try {
      localStorage.setItem(LABEL_FILTER_KEY, JSON.stringify(activeLabel ? [activeLabel] : []));
    } catch { /* private mode / quota — filtering still works this session */ }
  }, [activeLabel]);

  useEffect(() => {
    selectedRef.current = selectedName;
    // Only after the first restore: the mount render's null must not erase
    // the stored selection before fleet_list has had a chance to read it.
    if (restoredRef.current) writeKey(LAST_SELECTED_FLEET_KEY, selectedName);
  }, [selectedName]);

  useEffect(() => {
    setTab("overview");
  }, [selectedName]);

  // Report the selected fleet up (DashboardApp keeps it for the palette).
  useEffect(() => {
    onSelect?.(selectedName);
    return () => onSelect?.(null);
  }, [selectedName, onSelect]);

  async function loadList() {
    try {
      const rows = await invoke<FleetSummary[]>("fleet_list");
      setFleets(rows);
      // First load only: restore the last selection, else the first fleet.
      // Later reloads keep whatever is selected (the ref is non-null), and a
      // cleared selection (Esc → null) is never re-filled.
      if (selectedRef.current === null && !restoredRef.current && rows.length > 0) {
        restoredRef.current = true;
        const last = readKey(LAST_SELECTED_FLEET_KEY);
        const name = last && rows.some((r) => r.name === last) ? last : rows[0].name;
        selectedRef.current = name;
        setSelectedName(name);
      }
    } catch (err) {
      showToast(String(err), 4000);
    }
  }

  async function loadLabels() {
    try {
      setLabels(await invoke<LabelView[]>("fleet_labels_list"));
    } catch {
      setLabels([]); // registry unreadable — degrade to a flat, unlabelled list
    }
  }

  async function loadDetail(name: string) {
    try {
      const [d, j] = await Promise.all([
        invoke<Detail>("fleet_detail", { name }),
        invoke<JobRow[]>("fleet_jobs", { name, all: false }),
      ]);
      if (selectedRef.current !== name) return; // stale
      setDetail(d);
      setJobs(j);
    } catch (err) {
      showToast(String(err), 4000);
    }
  }

  // Initial list load + labels + agents map
  useEffect(() => {
    void loadList();
    void loadLabels();
    invoke<AgentEntry[]>("list_agents").then((agents) => {
      setAgentMap(new Map(agents.map((a) => [a.name, a])));
    }).catch(() => {});
  }, []);

  // Load detail whenever selection changes
  useEffect(() => {
    if (selectedName) {
      void loadDetail(selectedName);
    } else {
      setDetail(null);
      setJobs([]);
    }
  }, [selectedName]);

  useEffect(() => {
    if (!requestedName) return;
    // Sync the ref before the state so loadList's first-load branch (which
    // reads the ref when fleet_list resolves) cannot race ahead of this
    // selection on mount.
    selectedRef.current = requestedName;
    restoredRef.current = true;
    setSelectedName(requestedName);
    onRequestHandled?.();
  }, [requestedName, onRequestHandled]);

  // Refresh jobs when a fleet run completes
  useEffect(() => {
    const unlisten = listen<{ name: string; ok: boolean }>(
      "fleet:run_done",
      (event) => {
        const { name, ok } = event.payload;
        showToast(ok ? t("fleet.runDone") : t("fleet.runFailed"), 3000);
        void loadList();
        if (selectedRef.current === name) void loadDetail(name);
      },
    );
    return () => { void unlisten.then((fn) => fn()); };
  }, []);

  function handleRefresh() {
    void loadList();
    void loadLabels();
    if (selectedName) void loadDetail(selectedName);
  }

  function handleDelete() {
    setSelectedName(null);
    setDetail(null);
    setJobs([]);
    void loadList();
  }

  function handleCreated(name: string) {
    setShowCreate(false);
    void loadList().then(() => setSelectedName(name));
  }

  const rows: SourceRowData[] = fleets.map((f) => ({
    id: f.name,
    name: f.display_name,
    subtitle: t("fleet.rowSubtitle", { count: f.member_count }),
    status: fleetStatusOf(f),
    needsYou: f.active_jobs,
    avatar: <span className="fleet-avatar" aria-hidden="true"><Ico>{FLEET_GLYPH}</Ico></span>,
    facets: f.labels.length > 0 ? f.labels : [UNGROUPED],
  }));
  const ungrouped = fleets.filter((f) => f.labels.length === 0).length;
  const facets: SourceFacet[] = [
    ...labels.map((l) => ({ id: l.id, label: l.display || l.id, count: l.fleet_count })),
    ...(ungrouped > 0 ? [{ id: UNGROUPED, label: t("fleet.labelUngrouped"), count: ungrouped }] : []),
  ];
  const summary = fleets.find((f) => f.name === detail?.name);
  const cls = `master-detail master-detail--${listMode}${listShown ? " master-detail--list-shown" : ""}`;

  return (
    <div className={cls} style={{ ["--md-list-width" as string]: `${column.width}px` }}>
      <SourceList
        title={t("nav.fleets")}
        count={fleets.length}
        rows={rows}
        facets={facets}
        allLabel={t("fleet.labelAll")}
        activeFacet={activeLabel}
        onFacet={setActiveLabel}
        filter={filter}
        onFilter={setFilter}
        filterPlaceholder={t("fleet.filter")}
        selectedId={selectedName}
        onSelect={(id) => {
          setSelectedName(id);
          setListShown(false);
        }}
        onCreate={() => setShowCreate(true)}
        createLabel={t("fleet.new")}
        emptyState={<p className="source-list__empty">{fleets.length === 0 ? t("fleet.empty") : t("fleet.noMatch")}</p>}
      />
      <ListDivider column={column} label={t("shell.resizeList")} />
      <div className="master-detail__detail">
        {listMode === "overlay" && (
          <button
            type="button"
            className="btn btn--secondary master-detail__show-list"
            onClick={() => setListShown((v) => !v)}
          >
            {t("shell.showList")}
          </button>
        )}
        {detail && summary ? (
          <DetailPage
            key={detail.name}
            avatar={<span className="fleet-avatar fleet-avatar--lg" aria-hidden="true"><Ico>{FLEET_GLYPH}</Ico></span>}
            title={detail.display_name}
            status={fleetStatusOf(summary)}
            meta={fleetMeta(detail, t)}
            actions={<FleetHeader detail={detail} onRefresh={handleRefresh} onDelete={handleDelete} />}
            tabs={FLEET_TABS.map((id) => ({ id, label: t(FLEET_TAB_LABEL_KEY[id]) }))}
            activeTab={tab}
            onTab={setTab}
          >
            {tab === "overview" && <FleetOverview detail={detail} jobs={jobs} agentMap={agentMap} onGoTo={setTab} />}
            {tab === "members" && (
              <FleetMembers detail={detail} agentMap={agentMap} labels={labels} fleetLabels={summary.labels} onRefresh={handleRefresh} />
            )}
            {tab === "jobs" && <FleetJobs detail={detail} jobs={jobs} onRefresh={handleRefresh} />}
            {tab === "settings" && <FleetSettings detail={detail} onRefresh={handleRefresh} onDelete={handleDelete} />}
          </DetailPage>
        ) : (
          <div className="fleet-view__empty">
            <p>{t("fleet.empty")}</p>
          </div>
        )}
      </div>
      {showCreate && (
        <FleetCreateModal
          onCreated={handleCreated}
          onClose={() => setShowCreate(false)}
        />
      )}
    </div>
  );
}
