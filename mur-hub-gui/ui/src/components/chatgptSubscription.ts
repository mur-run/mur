/**
 * chatgptSubscription.ts — pure state model for the ChatGPT Subscription
 * provider panel. No React, no Tauri, no I/O: every decision the panel makes
 * about *what to show* lives here so it can be tested as a table.
 *
 * The DTOs mirror the Rust views in
 * `src-tauri/src/chatgpt_subscription/` (serde field names, snake_case).
 */

export type BillingMode = "subscription" | "usage_billed" | "local";

export interface ChatGPTAccount {
  cli_present: boolean;
  /** True only for a ChatGPT (subscription) login; an API-key login is false. */
  logged_in: boolean;
  auth_mode?: string | null;
  email?: string | null;
  plan_type?: string | null;
}

export interface ChatGPTModel {
  id: string;
  display_name: string;
  is_default: boolean;
  reasoning_efforts: string[];
  input_modalities: string[];
}

export interface GatewayStatus {
  installed: boolean;
  running: boolean;
  codex_hook: boolean;
  /** `chatgpt` / `apikey` / `missing`; only `chatgpt` is usable here. */
  credential_mode?: string | null;
  compression: boolean;
}

/** Everything the panel has fetched so far; `null` = not loaded yet. */
export interface ChatGPTStateInput {
  account: ChatGPTAccount | null;
  accountError: string | null;
  loginInProgress: boolean;
  gateway: GatewayStatus | null;
  models: ChatGPTModel[] | null;
  modelsError: string | null;
}

/** Why a gateway that exists is still not usable for this provider. */
export type GatewayProblem =
  | "not-running"
  | "hook-missing"
  | "credential-apikey"
  | "credential-missing";

export type ChatGPTPanelState =
  | { kind: "loading" }
  | { kind: "codex-missing" }
  | { kind: "logged-out" }
  | { kind: "login-in-progress" }
  | { kind: "account-unavailable"; message: string }
  | { kind: "gateway-missing"; account: ChatGPTAccount }
  | { kind: "gateway-stopped"; account: ChatGPTAccount; problem: GatewayProblem }
  | { kind: "models-loading"; account: ChatGPTAccount }
  | {
      kind: "ready";
      account: ChatGPTAccount;
      models: ChatGPTModel[];
      /** Set when `model/list` failed: the panel offers the unverified-id field. */
      modelsError: string | null;
    };

export function gatewayProblem(g: GatewayStatus): GatewayProblem | null {
  if (!g.running) return "not-running";
  if (!g.codex_hook) return "hook-missing";
  if (g.credential_mode === "apikey") return "credential-apikey";
  if (g.credential_mode !== "chatgpt") return "credential-missing";
  return null;
}

/**
 * Precedence, first match wins:
 * login in progress → missing CLI → logged out → account error → loading →
 * gateway missing → gateway stopped/unusable → models loading → ready.
 */
export function deriveChatGPTState(input: ChatGPTStateInput): ChatGPTPanelState {
  const { account, accountError, loginInProgress, gateway, models, modelsError } = input;
  if (loginInProgress) return { kind: "login-in-progress" };
  if (account && !account.cli_present) return { kind: "codex-missing" };
  if (account && !account.logged_in) return { kind: "logged-out" };
  if (accountError) return { kind: "account-unavailable", message: accountError };
  if (!account) return { kind: "loading" };
  if (!gateway) return { kind: "loading" };
  if (!gateway.installed) return { kind: "gateway-missing", account };
  const problem = gatewayProblem(gateway);
  if (problem) return { kind: "gateway-stopped", account, problem };
  if (models === null && !modelsError) return { kind: "models-loading", account };
  return { kind: "ready", account, models: models ?? [], modelsError };
}

export type BillingLabel = "Subscription" | "Usage billed" | "Local" | "Unknown";

/** Unknown stays "Unknown" — never "$0", never assumed free. */
export function billingLabel(mode?: BillingMode | string | null): BillingLabel {
  switch (mode) {
    case "subscription":
      return "Subscription";
    case "usage_billed":
      return "Usage billed";
    case "local":
      return "Local";
    default:
      return "Unknown";
  }
}

/** The registry alias a picked model gets by default: `chatgpt_<slug>`. */
export function defaultChatGPTAlias(modelId: string): string {
  const slug = modelId
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
  return `chatgpt_${slug}`;
}

export type PaidFallbackWarning =
  | "settings.modelSwitch.paidFallback"
  | "settings.modelSwitch.unknownFallback";

/**
 * Whether putting `fallback` behind a subscription `primary` changes who
 * pays. A usage-billed fallback is a real warning; unknown billing is a
 * neutral one — it does not claim safety. Both are advisory: the user may
 * still save the chain explicitly. `null` when there is nothing to say.
 */
/** Anything with an optional billing field — a ModelOption or a test stub. */
interface Billed {
  ref_name?: string;
  billing?: BillingMode | string | null;
}

export function paidFallbackWarning(
  primary: Billed | null | undefined,
  fallback: Billed | null | undefined,
): PaidFallbackWarning | null {
  if (primary?.billing !== "subscription" || !fallback) return null;
  if (fallback.billing === "usage_billed") return "settings.modelSwitch.paidFallback";
  if (fallback.billing === "subscription" || fallback.billing === "local") return null;
  return "settings.modelSwitch.unknownFallback";
}

/** i18n key for a billing badge. */
export function billingKey(
  mode?: BillingMode | string | null,
): "billing.subscription" | "billing.usageBilled" | "billing.local" | "billing.unknown" {
  switch (billingLabel(mode)) {
    case "Subscription":
      return "billing.subscription";
    case "Usage billed":
      return "billing.usageBilled";
    case "Local":
      return "billing.local";
    default:
      return "billing.unknown";
  }
}
