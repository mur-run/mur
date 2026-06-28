// mur-hub-gui/ui/src/components/fleet/FleetView.tsx
import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useT } from "../../i18n";
import type { FleetSummary, FleetDetail as Detail, JobRow } from "./types";
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

export function FleetView() {
  const { t } = useT();
  const [fleets, setFleets] = useState<FleetSummary[]>([]);
  const [selectedName, setSelectedName] = useState<string | null>(null);
  const [detail, setDetail] = useState<Detail | null>(null);
  const [jobs, setJobs] = useState<JobRow[]>([]);
  const [showCreate, setShowCreate] = useState(false);
  const selectedRef = useRef<string | null>(null);

  useEffect(() => {
    selectedRef.current = selectedName;
  }, [selectedName]);

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

  // Initial list load
  useEffect(() => {
    void loadList();
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

  return (
    <div className="fleet-view">
      <FleetRail
        fleets={fleets}
        selectedName={selectedName}
        onSelect={handleSelect}
        onNew={() => setShowCreate(true)}
      />
      <main className="fleet-view__main">
        {detail ? (
          <FleetDetail
            key={detail.name}
            detail={detail}
            jobs={jobs}
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
