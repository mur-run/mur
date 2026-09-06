import { useEffect, useMemo, useRef, useState } from "react";
import type { AgentEntry, AgentRuntimeStatus } from "../../types";
import type { ChannelSummary } from "../../work/types";
import { useAgents } from "../../context/AgentContext";
import { useT } from "../../i18n";
import { avatarPreset, familyOf } from "../../utils";
import { PetFace } from "../PetFace";
import { SourceList } from "../shell/SourceList";
import type { SourceFacet, SourceRowData } from "../shell/sourceListModel";
import { ListDivider } from "../shell/ListDivider";
import {
  LIST_WIDTH_DEFAULT, LIST_WIDTH_MAX, LIST_WIDTH_MIN, useResizableColumn,
} from "../shell/useResizableColumn";
import { statusOf } from "../shell/Status";
import { DirtyProvider, useDirtyGuard } from "../shell/dirty";
import { listModeFor } from "../shell/breakpoints";
import { useWindowWidth } from "../shell/useWindowWidth";
import { readKey, writeKey } from "../shell/persist";
import { AgentDetail } from "../detail/agent/AgentDetail";
import { AgentsOverview } from "./AgentsOverview";

/** Sentinel for the "no role assigned" facet. */
export const NO_ROLE = "__none__";
export const LAST_SELECTED_AGENT_KEY = "mur.agents.lastSelected";
export const AGENTS_LIST_WIDTH_KEY = "mur.agents.listWidth";

export interface AgentsPageProps {
  agents: AgentEntry[];
  runtimeMap: Map<string, AgentRuntimeStatus>;
  channels: ChannelSummary[];
  needsYou: Record<string, number>;
  selectedAgent: string | null;
  onNewAgent: () => void;
  onOpenChat: (name: string) => void;
  onOpenHome: () => void;
}

/** Facet chips by ROLE (persona category was useless — nearly every agent is
 *  "custom"): each distinct role plus a "no role" bucket. */
export function roleFacets(agents: AgentEntry[], noRoleLabel: string): SourceFacet[] {
  const counts: Record<string, number> = {};
  let noRole = 0;
  for (const a of agents) {
    const r = a.role?.trim();
    if (r) counts[r] = (counts[r] ?? 0) + 1;
    else noRole++;
  }
  const facets = Object.keys(counts)
    .sort((x, y) => x.localeCompare(y))
    .map((r) => ({ id: r, label: r, count: counts[r] }));
  if (noRole > 0) facets.push({ id: NO_ROLE, label: noRoleLabel, count: noRole });
  return facets;
}

/** Agents page (spec §3.1 / §4.5): source list | divider | detail. The
 *  DirtyProvider scopes unsaved-edit guards to this page. */
export function AgentsPage(props: AgentsPageProps) {
  return (
    <DirtyProvider>
      <AgentsPageInner {...props} />
    </DirtyProvider>
  );
}

function AgentsPageInner({
  agents, runtimeMap, channels, needsYou, selectedAgent, onNewAgent, onOpenChat, onOpenHome,
}: AgentsPageProps) {
  const { t } = useT();
  const { setSelected } = useAgents();
  const { confirmLeave } = useDirtyGuard();
  const [filter, setFilter] = useState("");
  const [facet, setFacet] = useState<string | null>(null);
  // Overlay list mode only (< 960 px): the list slides over the detail.
  const [listShown, setListShown] = useState(false);
  const column = useResizableColumn(AGENTS_LIST_WIDTH_KEY, LIST_WIDTH_DEFAULT, LIST_WIDTH_MIN, LIST_WIDTH_MAX);
  const listMode = listModeFor(useWindowWidth());

  // Restore the last selection ONCE, when agents first arrive (spec §6.1).
  // Guarded by a ref: re-running on every transition to null would re-select
  // the row the user just cleared with Esc.
  const restored = useRef(false);
  useEffect(() => {
    if (restored.current || agents.length === 0) return;
    restored.current = true;
    if (selectedAgent !== null) return;
    const last = readKey(LAST_SELECTED_AGENT_KEY);
    if (last && agents.some((a) => a.name === last)) setSelected(last);
  }, [agents, selectedAgent, setSelected]);
  useEffect(() => {
    writeKey(LAST_SELECTED_AGENT_KEY, selectedAgent);
  }, [selectedAgent]);

  async function select(name: string | null) {
    if (name === selectedAgent) return;
    if (await confirmLeave(t("detail.discardBody"), t("detail.discardTitle"))) {
      setSelected(name);
      setListShown(false);
    }
  }

  const rows: SourceRowData[] = useMemo(
    () =>
      agents.map((a) => {
        const preset = avatarPreset(a);
        return {
          id: a.name,
          name: a.display_name,
          subtitle: [a.role?.trim(), a.model_id].filter(Boolean).join(" · "),
          status: statusOf(runtimeMap.get(a.name)?.state),
          needsYou: needsYou[a.name] ?? 0,
          avatar: <PetFace presetId={preset} family={familyOf(preset)} expression="idle" size={28} />,
          facets: [a.role?.trim() || NO_ROLE],
        };
      }),
    [agents, runtimeMap, needsYou],
  );

  const entry = agents.find((a) => a.name === selectedAgent);
  const cls = `master-detail master-detail--${listMode}${listShown ? " master-detail--list-shown" : ""}`;

  return (
    <div className={cls} style={{ ["--md-list-width" as string]: `${column.width}px` }}>
      <SourceList
        title={t("nav.agents")}
        count={agents.length}
        rows={rows}
        facets={roleFacets(agents, t("dashboard.noRole"))}
        allLabel={t("dashboard.all")}
        activeFacet={facet}
        onFacet={setFacet}
        filter={filter}
        onFilter={setFilter}
        filterPlaceholder={t("agents.filter")}
        selectedId={selectedAgent}
        onSelect={(id) => {
          void select(id);
        }}
        onCreate={onNewAgent}
        createLabel={t("app.newAgent")}
        emptyState={<p className="source-list__empty">{t("agents.noMatch")}</p>}
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
        {selectedAgent && entry ? (
          // Keyed per agent: remounts the detail (and its dirty set) so the
          // cross-fade runs and stale form state never leaks across agents.
          <AgentDetail
            key={selectedAgent}
            agentName={selectedAgent}
            entry={entry}
            runtime={runtimeMap.get(selectedAgent)}
            channels={channels}
            needsYou={needsYou[selectedAgent] ?? 0}
            onOpenChat={onOpenChat}
            onOpenHome={onOpenHome}
          />
        ) : (
          <AgentsOverview agents={agents} runtimeMap={runtimeMap} onNewAgent={onNewAgent} />
        )}
      </div>
    </div>
  );
}
