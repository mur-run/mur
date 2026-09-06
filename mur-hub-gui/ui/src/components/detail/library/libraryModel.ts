import type { ReactNode } from "react";
import type { SourceFacet, SourceRowData } from "../../shell/sourceListModel";

// ── Backend shapes (mirror src-tauri) ────────────────────────────────────────
export interface InstalledSkillView {
  name: string;
  description: string;
  category: string;
  origin_version: string | null;
  status: string;
  agents: string[];
  path: string | null;
}
export interface InstalledMcpView {
  id: string;
  name: string;
  description: string;
  transport: string;
  agents: string[];
}
export interface AddonAgentState {
  agent: string;
  enabled: boolean;
}
export interface InstalledAddonAgg {
  id: string;
  source: string;
  skill_count: number;
  mcp_count: number;
  command_count: number;
  agents: AddonAgentState[];
}
export interface WorkflowView {
  name: string;
  description: string;
  path: string;
}

export type LibraryKind = "skill" | "mcp" | "plugin" | "workflow";

export interface LibraryItem {
  id: string;
  kind: LibraryKind;
  name: string;
  description?: string;
  meta: { label: string; value: string; mono?: boolean }[];
  path?: string | null;
}

/** One agent that uses the item. `enabled` undefined = no toggle offered. */
export interface LibraryAgentUse {
  agent: string;
  enabled?: boolean;
}

const SEP = " · ";
/** The backend's "not in the registry" status; not worth a word in the subtitle. */
const STATUS_NONE = "—";

function facetsOf(values: string[]): SourceFacet[] {
  const counts: Record<string, number> = {};
  for (const v of values) counts[v] = (counts[v] ?? 0) + 1;
  return Object.keys(counts)
    .sort((a, b) => a.localeCompare(b))
    .map((v) => ({ id: v, label: v, count: counts[v] }));
}

type Avatar<T> = (r: T) => ReactNode;

export function skillRows(skills: InstalledSkillView[], versionPrefix: string, avatar: Avatar<InstalledSkillView>): SourceRowData[] {
  return skills.map((s) => ({
    id: s.name,
    name: s.name,
    subtitle: [
      s.category,
      s.origin_version ? `${versionPrefix}${s.origin_version}` : null,
      s.status !== STATUS_NONE ? s.status : null,
    ]
      .filter(Boolean)
      .join(SEP),
    avatar: avatar(s),
    facets: [s.category],
  }));
}
export const skillFacets = (skills: InstalledSkillView[]): SourceFacet[] => facetsOf(skills.map((s) => s.category));

export function mcpRows(servers: InstalledMcpView[], usedBy: (n: number) => string, avatar: Avatar<InstalledMcpView>): SourceRowData[] {
  return servers.map((s) => ({
    id: s.id,
    name: s.name,
    subtitle: [s.transport, usedBy(s.agents.length)].join(SEP),
    avatar: avatar(s),
    facets: [s.transport],
  }));
}
export const mcpFacets = (servers: InstalledMcpView[]): SourceFacet[] => facetsOf(servers.map((s) => s.transport));

export function pluginRows(
  addons: InstalledAddonAgg[],
  labels: { skills: string; mcp: string; commands: string },
  avatar: Avatar<InstalledAddonAgg>,
): SourceRowData[] {
  return addons.map((a) => ({
    id: a.id,
    name: a.id,
    subtitle: [`${a.skill_count} ${labels.skills}`, `${a.mcp_count} ${labels.mcp}`, `${a.command_count} ${labels.commands}`].join(SEP),
    avatar: avatar(a),
    facets: [],
  }));
}

export function workflowRows(workflows: WorkflowView[], avatar: Avatar<WorkflowView>): SourceRowData[] {
  return workflows.map((w) => ({
    id: w.path,
    name: w.name,
    subtitle: w.path.split("/").pop() ?? w.path,
    avatar: avatar(w),
    facets: [],
  }));
}

type MetaLabels = Record<string, string>;

/** The detail's view of one record: meta rows in display order, path when known. */
export function itemFor(kind: "skill", r: InstalledSkillView, l: MetaLabels): LibraryItem;
export function itemFor(kind: "mcp", r: InstalledMcpView, l: MetaLabels): LibraryItem;
export function itemFor(kind: "plugin", r: InstalledAddonAgg, l: MetaLabels): LibraryItem;
export function itemFor(kind: "workflow", r: WorkflowView, l: MetaLabels): LibraryItem;
export function itemFor(
  kind: LibraryKind,
  r: InstalledSkillView | InstalledMcpView | InstalledAddonAgg | WorkflowView,
  l: MetaLabels,
): LibraryItem {
  switch (kind) {
    case "skill": {
      const s = r as InstalledSkillView;
      return {
        id: s.name,
        kind,
        name: s.name,
        description: s.description,
        path: s.path,
        meta: [
          { label: l.category, value: s.category },
          { label: l.version, value: s.origin_version ?? STATUS_NONE },
          { label: l.status, value: s.status },
          { label: l.path, value: s.path ?? STATUS_NONE, mono: true },
        ],
      };
    }
    case "mcp": {
      const s = r as InstalledMcpView;
      return {
        id: s.id,
        kind,
        name: s.name,
        description: s.description,
        meta: [
          { label: l.transport, value: s.transport },
          { label: l.serverId, value: s.id, mono: true },
        ],
      };
    }
    case "plugin": {
      const a = r as InstalledAddonAgg;
      return {
        id: a.id,
        kind,
        name: a.id,
        path: a.source,
        meta: [
          { label: l.source, value: a.source, mono: true },
          { label: l.skills, value: String(a.skill_count) },
          { label: l.mcp, value: String(a.mcp_count) },
          { label: l.commands, value: String(a.command_count) },
        ],
      };
    }
    case "workflow": {
      const w = r as WorkflowView;
      return {
        id: w.path,
        kind,
        name: w.name,
        description: w.description,
        path: w.path,
        meta: [{ label: l.path, value: w.path, mono: true }],
      };
    }
  }
}
