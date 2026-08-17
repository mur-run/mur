/**
 * Pure TypeScript helpers for the model picker.
 * No DOM, no React — unit-testable.
 */

export interface ModelOption {
  ref_name: string;
  provider: string;
  /** Who makes the model, when that differs from the wire protocol. */
  vendor?: string | null;
  model: string;
  tier?: string;
  input_cost?: number;
  output_cost?: number;
  context_window?: number;
  capabilities: string[];
}

/**
 * Filter models by term (searches ref_name, provider, model case-insensitively).
 * Returns all models if term is empty after trimming.
 */
export function filterModels(models: ModelOption[], term: string): ModelOption[] {
  const t = term.trim().toLowerCase();
  if (!t) return models;

  return models.filter(
    (m) =>
      m.ref_name.toLowerCase().includes(t) ||
      m.provider.toLowerCase().includes(t) ||
      m.model.toLowerCase().includes(t)
  );
}

/**
 * Group models by provider.
 * Returns an array of [provider_name, models_in_group] tuples.
 */
export function groupByProvider(models: ModelOption[]): [string, ModelOption[]][] {
  const map = new Map<string, ModelOption[]>();

  for (const m of models) {
    const arr = map.get(m.provider) ?? [];
    arr.push(m);
    map.set(m.provider, arr);
  }

  return [...map.entries()];
}

/**
 * Format input or output cost in millions.
 * Returns null if cost is undefined or null (unknown cost).
 * Returns "$X/M" for known costs (e.g., "$5/M" for 0.005).
 */
export function formatCost(perK?: number | null): string | null {
  if (perK === undefined || perK === null || !Number.isFinite(perK)) return null;

  // Convert per-1K to per-1M (multiply by 1000)
  const perMillion = perK * 1000;

  return `$${perMillion.toLocaleString("en-US", {
    minimumFractionDigits: 0,
    maximumFractionDigits: 1,
  })}/M`;
}
