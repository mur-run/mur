/**
 * modelLibrary.ts — pure helpers for the Model Library surface.
 * No React, no side effects, no I/O. Safe to import in tests directly.
 */

export interface CloudPreset {
  key: string;
  name: string;
  baseUrl: string;
  logo: string;
  color: string;
}

export const CLOUD_PRESETS: CloudPreset[] = [
  {
    key: "anthropic",
    name: "Anthropic (Claude)",
    baseUrl: "https://api.anthropic.com/v1",
    logo: "A",
    color: "#C5694A",
  },
  {
    key: "openai",
    name: "OpenAI",
    baseUrl: "https://api.openai.com/v1",
    logo: "AI",
    color: "#10A37F",
  },
  {
    key: "google",
    name: "Google (Gemini)",
    baseUrl: "https://generativelanguage.googleapis.com/v1beta",
    logo: "G",
    color: "#E8543F",
  },
  {
    key: "openrouter",
    name: "OpenRouter",
    baseUrl: "https://openrouter.ai/api/v1",
    logo: "OR",
    color: "#8B5CF6",
  },
  {
    key: "xai",
    name: "xAI (Grok)",
    baseUrl: "https://api.x.ai/v1",
    logo: "X",
    color: "#111111",
  },
  {
    key: "mistral",
    name: "Mistral",
    baseUrl: "https://api.mistral.ai/v1",
    logo: "M",
    color: "#FF7000",
  },
  {
    key: "deepseek",
    name: "DeepSeek",
    baseUrl: "https://api.deepseek.com/v1",
    logo: "DS",
    color: "#4A6CF0",
  },
  {
    key: "groq",
    name: "Groq",
    baseUrl: "https://api.groq.com/openai/v1",
    logo: "GQ",
    color: "#F55036",
  },
  {
    key: "together",
    name: "Together AI",
    baseUrl: "https://api.together.xyz/v1",
    logo: "T",
    color: "#0F6FFF",
  },
  {
    key: "fireworks",
    name: "Fireworks AI",
    baseUrl: "https://api.fireworks.ai/inference/v1",
    logo: "FW",
    color: "#9215FF",
  },
  {
    key: "cohere",
    name: "Cohere",
    baseUrl: "https://api.cohere.ai/compatibility/v1",
    logo: "CO",
    color: "#3B82F6",
  },
  {
    key: "custom",
    name: "Custom (OpenAI-compat)",
    baseUrl: "https://",
    logo: "+",
    color: "#334155",
  },
];

// Azure OpenAI is intentionally NOT a preset here: its `api-key` auth header
// and deployment-scoped URLs are not the generic OpenAI-compatible shape
// model_discovery speaks. Anthropic IS a preset — discover_models_for() sends
// its `x-api-key` + `anthropic-version` headers.

/**
 * Pure immutable selection toggle — does not mutate the input Set.
 * If `id` is already in `sel`, returns a new Set without it.
 * Otherwise, returns a new Set with `id` added.
 */
export function togglePick(sel: Set<string>, id: string): Set<string> {
  const next = new Set(sel);
  if (next.has(id)) {
    next.delete(id);
  } else {
    next.add(id);
  }
  return next;
}
