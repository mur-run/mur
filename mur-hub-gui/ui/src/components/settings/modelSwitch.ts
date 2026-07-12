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
