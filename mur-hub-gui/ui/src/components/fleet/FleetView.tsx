// mur-hub-gui/ui/src/components/fleet/FleetView.tsx
import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useT } from "../../i18n";
import type { AgentEntry } from "../../types";
import type { FleetSummary, FleetDetail as Detail, JobRow, LabelView } from "./types";
import { FleetRail } from "./FleetRail";
import { FleetDetail } from "./FleetDetail";
import { FleetCreateModal } from "./FleetCreateModal";

function showToast(msg: string, durationMs = 2500) {
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = msg;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), durationMs);
}

const LABEL_FILTER_KEY = "mur.fleet.labelFilter";

function loadLabelFilter(): string[] {
  try {
    const raw = localStorage.getItem(LABEL_FILTER_KEY);
    const parsed = raw ? JSON.parse(raw) : null;
    return Array.isArray(parsed) ? parsed.filter((x) => typeof x === "string") : [];
  } catch {
    return [];
  }
}

export function FleetView({ query, onSelect }: { query?: string; onSelect?: (name: string | null) => void }) {
  const { t } = useT();
  const [fleets, setFleets] = useState<FleetSummary[]>([]);
  const [selectedName, setSelectedName] = useState<string | null>(null);
  const [detail, setDetail] = useState<Detail | null>(null);
  const [jobs, setJobs] = useState<JobRow[]>([]);
  const [showCreate, setShowCreate] = useState(false);
  const [agentMap, setAgentMap] = useState<Map<string, AgentEntry>>(new Map());
  const [labels, setLabels] = useState<LabelView[]>([]);
  const [selectedLabels, setSelectedLabels] = useState<string[]>(loadLabelFilter);
  const selectedRef = useRef<string | null>(null);

  // Persist the chip selection so the rail comes back the way it was left.
  useEffect(() => {
    try {
      localStorage.setItem(LABEL_FILTER_KEY, JSON.stringify(selectedLabels));
    } catch { /* private mode / quota — filtering still works this session */ }
  }, [selectedLabels]);

  useEffect(() => {
    selectedRef.current = selectedName;
  }, [selectedName]);

  // Report the selected fleet up so DashboardApp can show the FleetInspector.
  useEffect(() => {
    onSelect?.(selectedName);
    return () => onSelect?.(null);
  }, [selectedName, onSelect]);

  async function loadList() {
    try {
      const rows = await invoke<FleetSummary[]>("fleet_list");
      setFleets(rows);
      // Auto-select first on initial load
      if (selectedRef.current === null && rows.length > 0) {
        setSelectedName(rows[0].name);
      }
    } catch (err) {
      showToast(String(err), 4000);
    }
  }

  async function loadLabels() {
    try {
      setLabels(await invoke<LabelView[]>("fleet_labels_list"));
    } catch {
      setLabels([]); // registry unreadable — degrade to a flat, unlabelled rail
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

  // Refresh jobs when a fleet run completes
  useEffect(() => {
    const unlisten = listen<{ name: string; ok: boolean }>(
      "fleet:run_done",
      (event) => {
        const { name, ok } = event.payload;
        showToast(ok ? t("fleet.runDone") : t("fleet.runFailed"), 3000);
        void loadList();
        if (selectedRef.current === name) void loadDetail(name);
      }
    );
    return () => { void unlisten.then((fn) => fn()); };
  }, []);

  function handleSelect(name: string) {
    setSelectedName(name);
  }

  function handleRefresh() {
    void loadList();
    void loadLabels();
    if (selectedName) void loadDetail(selectedName);
  }

  function toggleLabel(id: string) {
    setSelectedLabels((prev) =>
      prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id]
    );
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

  return (
    <div className="fleet-view">
      <FleetRail
        fleets={query ? fleets.filter((f) => {
          const q = query.toLowerCase();
          return f.name.toLowerCase().includes(q) || f.display_name.toLowerCase().includes(q) || f.goal.toLowerCase().includes(q);
        }) : fleets}
        selectedName={selectedName}
        labels={labels}
        selectedLabels={selectedLabels}
        onToggleLabel={toggleLabel}
        onClearLabels={() => setSelectedLabels([])}
        onSelect={handleSelect}
        onNew={() => setShowCreate(true)}
      />
      <main className="fleet-view__main">
        {detail ? (
          <FleetDetail
            key={detail.name}
            detail={detail}
            jobs={jobs}
            agentMap={agentMap}
            labels={labels}
            running={fleets.find((f) => f.name === detail.name)?.running ?? false}
            fleetLabels={fleets.find((f) => f.name === detail.name)?.labels ?? []}
            onRefresh={handleRefresh}
            onDelete={handleDelete}
          />
        ) : (
          <div className="fleet-view__empty">
            <p>{t("fleet.empty")}</p>
          </div>
        )}
      </main>
      {showCreate && (
        <FleetCreateModal
          onCreated={handleCreated}
          onClose={() => setShowCreate(false)}
        />
      )}
    </div>
  );
}
