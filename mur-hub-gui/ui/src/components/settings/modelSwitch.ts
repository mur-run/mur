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
// Mirrors mur_common::config::ModelSwitchConfig over the Tauri boundary.
export interface ModelSwitchView {
  default: string | null;
  fallback_chain: string[];
  retry: RetryView;
  routing: RoutingView;
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
 * serialized, so they pass through untouched.
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
  };
}

