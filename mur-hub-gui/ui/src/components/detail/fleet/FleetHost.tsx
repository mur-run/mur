import { useCallback, useEffect, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AgentEntry } from "../../../types";
import { useAgents } from "../../../context/AgentContext";
import type { FleetSummary, LabelView } from "../../fleet/types";
import type { FleetTabId } from "../../shell/detailTabs";
import { FleetDetailPane } from "./FleetDetailPane";

export interface FleetHostProps {
  name: string;
  /** Tab to open on. Default Overview. */
  initialTab?: FleetTabId;
  onDeleted: () => void;
  /** Rendered when fleet_list does not list `name`. */
  missing: ReactNode;
  onOpenInWindow?: () => void;
  /** The fleet's display name once fleet_list answers (the peek's title). */
  onTitle?: (displayName: string) => void;
}

/** Everything FleetDetailPane needs from outside a Fleets page: the summary
 *  (status + labels) from fleet_list, the label registry, and the agent map
 *  from the AgentProvider. Shared by the detail window and the Home peek. */
export function FleetHost({ name, initialTab, onDeleted, missing, onOpenInWindow, onTitle }: FleetHostProps) {
  const { agents } = useAgents();
  const [fleets, setFleets] = useState<FleetSummary[] | null>(null);
  const [labels, setLabels] = useState<LabelView[]>([]);
  const [agentMap, setAgentMap] = useState<Map<string, AgentEntry>>(new Map());

  useEffect(() => {
    setAgentMap(new Map(agents.map((a) => [a.name, a])));
  }, [agents]);

  const load = useCallback(() => {
    invoke<FleetSummary[]>("fleet_list").then(setFleets).catch(() => setFleets([]));
    invoke<LabelView[]>("fleet_labels_list").then(setLabels).catch(() => setLabels([]));
  }, []);
  useEffect(load, [load]);

  const displayName = fleets?.find((f) => f.name === name)?.display_name;
  useEffect(() => {
    if (displayName) onTitle?.(displayName);
  }, [displayName, onTitle]);

  if (fleets === null) return null;
  const summary = fleets.find((f) => f.name === name);
  if (!summary) return <>{missing}</>;
  return (
    <FleetDetailPane
      name={name}
      summary={summary}
      labels={labels}
      agentMap={agentMap}
      onRefresh={load}
      onDeleted={onDeleted}
      onOpenInWindow={onOpenInWindow}
      initialTab={initialTab}
    />
  );
}
