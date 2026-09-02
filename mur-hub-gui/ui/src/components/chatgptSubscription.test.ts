import { describe, it, expect } from "vitest";
import {
  billingLabel,
  defaultChatGPTAlias,
  deriveChatGPTState,
  type ChatGPTAccount,
  type ChatGPTStateInput,
  type GatewayStatus,
} from "./chatgptSubscription";

const chatgpt: ChatGPTAccount = {
  cli_present: true,
  logged_in: true,
  auth_mode: "chatgpt",
  email: "u@example.com",
  plan_type: "pro",
};
const ready: GatewayStatus = {
  installed: true,
  running: true,
  codex_hook: true,
  credential_mode: "chatgpt",
  compression: false,
};
const model = {
  id: "gpt-5.6-sol",
  display_name: "Sol",
  is_default: true,
  reasoning_efforts: ["low"],
  input_modalities: ["text"],
};
const base: ChatGPTStateInput = {
  account: chatgpt,
  accountError: null,
  loginInProgress: false,
  gateway: ready,
  models: [model],
  modelsError: null,
};

describe("deriveChatGPTState precedence", () => {
  const table: Array<[string, Partial<ChatGPTStateInput>, string]> = [
    ["login wins over everything", { loginInProgress: true, account: null, accountError: "x" }, "login-in-progress"],
    ["missing CLI", { account: { cli_present: false, logged_in: false }, accountError: "x" }, "codex-missing"],
    ["logged out", { account: { cli_present: true, logged_in: false }, accountError: "x" }, "logged-out"],
    ["account error", { account: null, accountError: "app-server died" }, "account-unavailable"],
    ["nothing loaded", { account: null, gateway: null }, "loading"],
    ["gateway not probed yet", { gateway: null }, "loading"],
    ["gateway missing", { gateway: { ...ready, installed: false, running: false } }, "gateway-missing"],
    ["gateway stopped", { gateway: { ...ready, running: false } }, "gateway-stopped"],
    ["models loading", { models: null }, "models-loading"],
    ["ready", {}, "ready"],
    ["ready with a model/list failure", { models: null, modelsError: "boom" }, "ready"],
  ];
  for (const [name, patch, kind] of table) {
    it(name, () => {
      expect(deriveChatGPTState({ ...base, ...patch }).kind).toBe(kind);
    });
  }

  it("carries the account error message", () => {
    const s = deriveChatGPTState({ ...base, account: null, accountError: "app-server died" });
    expect(s).toEqual({ kind: "account-unavailable", message: "app-server died" });
  });

  it("a failed model/list is ready with no models and the error kept", () => {
    const s = deriveChatGPTState({ ...base, models: null, modelsError: "boom" });
    expect(s).toEqual({ kind: "ready", account: chatgpt, models: [], modelsError: "boom" });
  });
});

describe("subscription readiness is strict", () => {
  it("an API-key Codex login never becomes subscription-ready", () => {
    const s = deriveChatGPTState({
      ...base,
      account: { cli_present: true, logged_in: false, auth_mode: "apiKey" },
    });
    expect(s.kind).toBe("logged-out");
  });

  it("a running gateway on an API key, without the hook, or without a credential is not usable", () => {
    for (const [patch, problem] of [
      [{ credential_mode: "apikey" }, "credential-apikey"],
      [{ credential_mode: "missing" }, "credential-missing"],
      [{ credential_mode: null }, "credential-missing"],
      [{ codex_hook: false }, "hook-missing"],
      [{ running: false }, "not-running"],
    ] as const) {
      const s = deriveChatGPTState({ ...base, gateway: { ...ready, ...patch } });
      expect(s).toEqual({ kind: "gateway-stopped", account: chatgpt, problem });
    }
  });
});

describe("billingLabel", () => {
  it("maps the three modes to distinct labels and everything else to Unknown", () => {
    expect(billingLabel("subscription")).toBe("Subscription");
    expect(billingLabel("usage_billed")).toBe("Usage billed");
    expect(billingLabel("local")).toBe("Local");
    expect(billingLabel(undefined)).toBe("Unknown");
    expect(billingLabel(null)).toBe("Unknown");
    expect(billingLabel("")).toBe("Unknown");
    expect(billingLabel("free")).toBe("Unknown");
    expect(new Set([billingLabel("subscription"), billingLabel("usage_billed"), billingLabel("local")]).size).toBe(3);
  });
});

describe("defaultChatGPTAlias", () => {
  it("slugs the model id under a chatgpt_ prefix", () => {
    expect(defaultChatGPTAlias("gpt-5.6-sol")).toBe("chatgpt_gpt_5_6_sol");
    expect(defaultChatGPTAlias("  GPT-5.6 Mini  ")).toBe("chatgpt_gpt_5_6_mini");
  });
});
