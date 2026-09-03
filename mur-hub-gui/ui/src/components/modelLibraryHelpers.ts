/**
 * modelLibrary.ts — pure helpers for the Model Library surface.
 * No React, no side effects, no I/O. Safe to import in tests directly.
 */

import type { TranslationKey } from "../i18n/types";
import type { GatewayReadiness } from "./chatgptSubscription";
import { CHATGPT_READINESS, CLAUDE_READINESS } from "./chatgptSubscription";

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

export type SubscriptionCopyKey =
  | "name"
  | "subtitle"
  | "billingNote"
  | "cliMissing"
  | "cliInstallHint"
  | "loggedOut"
  | "loggedOutApiBilled"
  | "loginBtn"
  | "loginInProgress"
  | "loginFailed"
  | "accountUnavailable"
  | "modelsTitle"
  | "modelsHint"
  | "registryTitle"
  | "disconnectBtn"
  | "disconnectHint"
  | "logoutBtn"
  | "logoutConfirmTitle"
  | "logoutConfirmBody"
  | "logoutConfirmOk";

export type SubscriptionCopy = Record<SubscriptionCopyKey, TranslationKey>;

/**
 * A subscription provider is deliberately not a CLOUD_PRESET: it has no API
 * key, no base URL to edit, and must never be tested against the vendor
 * host. Each one gets its own rail entry and shares SubscriptionProviderPanel.
 */
export interface SubscriptionDescriptor {
  key: string;
  /** The wire provider its registry entries carry. */
  provider: string;
  name: string;
  logo: string;
  color: string;
  readiness: GatewayReadiness;
  commands: {
    accountRead: string;
    modelsList: string;
    login: string;
    logout: string;
    modelsAdd: string;
    disconnect: string;
  };
  copy: SubscriptionCopy;
  /** Shown verbatim beside `copy.cliInstallHint`. */
  cliInstallCmd: string;
}

export const CHATGPT_SUBSCRIPTION: SubscriptionDescriptor = {
  key: "chatgpt-subscription",
  provider: "codex",
  name: "ChatGPT Subscription",
  logo: "GPT",
  color: "#10A37F",
  readiness: CHATGPT_READINESS,
  commands: {
    accountRead: "chatgpt_account_read",
    modelsList: "chatgpt_models_list",
    login: "chatgpt_login",
    logout: "chatgpt_logout",
    modelsAdd: "chatgpt_models_add",
    disconnect: "chatgpt_disconnect",
  },
  copy: {
    name: "lib.chatgpt.name",
    subtitle: "lib.chatgpt.subtitle",
    billingNote: "lib.chatgpt.billingNote",
    cliMissing: "lib.chatgpt.codexMissing",
    cliInstallHint: "lib.chatgpt.codexInstallHint",
    loggedOut: "lib.chatgpt.loggedOut",
    loggedOutApiBilled: "lib.chatgpt.loggedOutApiBilled",
    loginBtn: "lib.chatgpt.loginBtn",
    loginInProgress: "lib.chatgpt.loginInProgress",
    loginFailed: "lib.chatgpt.loginFailed",
    accountUnavailable: "lib.chatgpt.accountUnavailable",
    modelsTitle: "lib.chatgpt.modelsTitle",
    modelsHint: "lib.chatgpt.modelsHint",
    registryTitle: "lib.chatgpt.registryTitle",
    disconnectBtn: "lib.chatgpt.disconnectBtn",
    disconnectHint: "lib.chatgpt.disconnectHint",
    logoutBtn: "lib.chatgpt.logoutBtn",
    logoutConfirmTitle: "lib.chatgpt.logoutConfirmTitle",
    logoutConfirmBody: "lib.chatgpt.logoutConfirmBody",
    logoutConfirmOk: "lib.chatgpt.logoutConfirmOk",
  },
  cliInstallCmd: "npm install -g @openai/codex",
};

export const CLAUDE_SUBSCRIPTION: SubscriptionDescriptor = {
  key: "claude-subscription",
  provider: "claude",
  name: "Claude Subscription",
  logo: "CL",
  color: "#C5694A",
  readiness: CLAUDE_READINESS,
  commands: {
    accountRead: "claude_account_read",
    modelsList: "claude_models_list",
    login: "claude_login",
    logout: "claude_logout",
    modelsAdd: "claude_models_add",
    disconnect: "claude_disconnect",
  },
  copy: {
    name: "lib.claude.name",
    subtitle: "lib.claude.subtitle",
    billingNote: "lib.claude.billingNote",
    cliMissing: "lib.claude.cliMissing",
    cliInstallHint: "lib.claude.cliInstallHint",
    loggedOut: "lib.claude.loggedOut",
    loggedOutApiBilled: "lib.claude.loggedOutApiBilled",
    loginBtn: "lib.claude.loginBtn",
    loginInProgress: "lib.claude.loginInProgress",
    loginFailed: "lib.claude.loginFailed",
    accountUnavailable: "lib.claude.accountUnavailable",
    modelsTitle: "lib.claude.modelsTitle",
    modelsHint: "lib.claude.modelsHint",
    registryTitle: "lib.claude.registryTitle",
    disconnectBtn: "lib.claude.disconnectBtn",
    disconnectHint: "lib.claude.disconnectHint",
    logoutBtn: "lib.claude.logoutBtn",
    logoutConfirmTitle: "lib.claude.logoutConfirmTitle",
    logoutConfirmBody: "lib.claude.logoutConfirmBody",
    logoutConfirmOk: "lib.claude.logoutConfirmOk",
  },
  cliInstallCmd: "npm install -g @anthropic-ai/claude-code",
};

export const SUBSCRIPTION_PROVIDERS: readonly SubscriptionDescriptor[] = [
  CHATGPT_SUBSCRIPTION,
  CLAUDE_SUBSCRIPTION,
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
