/**
 * Pure TypeScript helpers for the Settings › Models slot pickers.
 * No DOM, no React — unit-testable.
 */

import type { ModelOption } from "./modelPicker";
import type { DetectedLocalView } from "./ModelLibraryPanels";

export interface SlotOptionGroup {
  label: string;
  options: SlotOption[];
}

export interface SlotOption {
  label: string;
  payload: SlotSelection;
}

export type SlotSelection =
  | { kind: "registry"; ref_name: string }
  | { kind: "local"; provider: string; model: string; base_url: string; dims: number | null };

function capitalize(s: string): string {
  return s.length ? s[0].toUpperCase() + s.slice(1) : s;
}

/**
 * One group per cloud provider from the registry, then one group per
 * detected local provider. Local group payloads route through "ollama"
 * for the ollama backend or "openai" (openai-compat) for everything else
 * detected locally (mlx, lmstudio, ...) — the only two providers the
 * backend factory accepts for local endpoints.
 */
export function buildSlotGroups(registry: ModelOption[], local: DetectedLocalView[]): SlotOptionGroup[] {
  const byProvider = new Map<string, ModelOption[]>();
  for (const m of registry) {
    const arr = byProvider.get(m.provider) ?? [];
    arr.push(m);
    byProvider.set(m.provider, arr);
  }

  const registryGroups: SlotOptionGroup[] = [...byProvider.entries()].map(([provider, models]) => ({
    label: capitalize(provider),
    options: models.map((m) => ({
      label: m.ref_name,
      payload: { kind: "registry", ref_name: m.ref_name },
    })),
  }));

  const localGroups: SlotOptionGroup[] = local.map((p) => ({
    label: `${p.name} (local)`,
    options: p.models.map((m) => ({
      label: m.model,
      payload: {
        kind: "local",
        provider: p.key === "ollama" ? "ollama" : "openai",
        model: m.model,
        base_url: p.base_url,
        dims: null,
      },
    })),
  }));

  return [...registryGroups, ...localGroups];
}

export function encodeSel(s: SlotSelection): string {
  return JSON.stringify(s);
}

export function decodeSel(v: string): SlotSelection {
  return JSON.parse(v);
}
