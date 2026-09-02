// Types + pure helpers for the global model-switch settings section.
export interface RoutingView {
  enabled: boolean;
  cheap: string | null;
  frontier: string | null;
  threshold_input_tokens: number | null;
}
export interface RetryView {
  max_retries: number;
  backoff_base_ms: number;
  cooldown_secs: number;
}
export interface SmartView {
  enabled: boolean;
  cheap: string | null;
  max_escalations: number;
}
// Mirrors mur_common::config::ModelSwitchConfig over the Tauri boundary.
export interface ModelSwitchView {
  default: string | null;
  fallback_chain: string[];
  retry: RetryView;
  routing: RoutingView;
  smart: SmartView;
}

/** Drop blank/whitespace refs and de-duplicate, preserving first-seen order. */
export function sanitizeChain(chain: string[]): string[] {
  const out: string[] = [];
  for (const raw of chain) {
    const r = raw.trim();
    if (r && !out.includes(r)) out.push(r);
  }
  return out;
}

/** Every ref must be a known model id (fail-closed mirror of the Rust guard). */
export function isChainValid(chain: string[], known: Set<string>): boolean {
  return chain.every((r) => known.has(r));
}

/**
 * Normalize a raw `model_switch_get` payload into a fully-populated view.
 * The Rust `ModelSwitchConfig` omits `default`, `fallback_chain`, and the
 * routing `cheap`/`frontier`/`threshold_input_tokens` fields when they are
 * unset/empty (`skip_serializing_if`), so they arrive `undefined` over the
 * Tauri boundary. Fill them so the UI never dereferences `undefined`
 * (e.g. `fallback_chain.length`). `retry` and `routing.enabled` are always
 * serialized, so they pass through untouched. `smart.cheap` is likewise
 * omitted when unset; `smart.enabled`/`smart.max_escalations` are always
 * serialized by the Rust side, but the whole `smart` object is guarded too
 * in case an older config predates the field entirely. The `enabled` fallback
 * mirrors the Rust default, which is off — Smart background routing is opt-in.
 */
export function normalizeMs(raw: ModelSwitchView): ModelSwitchView {
  return {
    ...raw,
    default: raw.default ?? null,
    fallback_chain: raw.fallback_chain ?? [],
    routing: {
      ...raw.routing,
      cheap: raw.routing?.cheap ?? null,
      frontier: raw.routing?.frontier ?? null,
      threshold_input_tokens: raw.routing?.threshold_input_tokens ?? null,
    },
    smart: {
      ...raw.smart,
      enabled: raw.smart?.enabled ?? false,
      cheap: raw.smart?.cheap ?? null,
      max_escalations: raw.smart?.max_escalations ?? 1,
    },
  };
}


import { paidFallbackWarning } from "../chatgptSubscription";

/**
 * The ref "+ Add fallback" pre-selects: the first option not already in the
 * chain, preferring one that does not move a subscription primary onto a
 * paid bill. The row stays editable — this only decides the *default*, so
 * MUR never inserts a paid fallback the user did not pick.
 */
export function pickNextFallback(
  chain: string[],
  options: Array<{ ref_name: string; billing?: string | null }>,
  primaryRef?: string | null,
): string | undefined {
  const primary = options.find((o) => o.ref_name === primaryRef) ?? null;
  const unused = options.filter((o) => !chain.includes(o.ref_name));
  return (
    unused.find((o) => paidFallbackWarning(primary, o) === null)?.ref_name ??
    unused[0]?.ref_name ??
    options[0]?.ref_name
  );
}
