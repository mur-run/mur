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
    key: "custom",
    name: "Custom (OpenAI-compat)",
    baseUrl: "https://",
    logo: "+",
    color: "#334155",
  },
];

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
