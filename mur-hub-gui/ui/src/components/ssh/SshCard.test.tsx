// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { LanguageProvider } from "../../i18n";
import type { PlanView } from "./sshApi";

const FP = (c: string) => `SHA256:${c.repeat(43)}`;

const plan: PlanView = {
  plan_id: "p1",
  agent: "qa",
  host: "git.example",
  user: "deploy",
  reason: null,
  keys: [{ fingerprint: FP("a"), comment: "me@laptop", loaded: true, add_cmd: null }],
  scanned: [{ fingerprint: FP("c"), key_type: "ssh-ed25519", matched: false }],
  verdict: "unknown",
  ticks_offered: true,
  spawn: "add_ssh",
  consent_text: null,
  differs_cmd: null,
  dedicated: { state: "off", reason: null, fingerprint: null, line: null },
};

const invoke = vi.fn((cmd: string) => Promise.resolve(cmd === "agent_ssh_plan" ? plan : null));
vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string) => invoke(cmd) }));

const { SshCard } = await import("./SshCard");

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
// jsdom here has no usable localStorage; LanguageProvider reads it on mount.
Object.defineProperty(globalThis, "localStorage", {
  configurable: true,
  value: { getItem: () => null, setItem: () => {}, removeItem: () => {}, clear: () => {}, key: () => null, length: 0 },
});

let host: HTMLElement;
let root: Root;

beforeEach(async () => {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  await act(async () => {
    root.render(
      <LanguageProvider>
        <SshCard target={{ agent: "qa", host: "git.example", port: null, user: "deploy" }} onClose={() => {}} />
      </LanguageProvider>,
    );
  });
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

// jsdom does no layout, so these only pin the hooks ssh.css hangs on; the
// overflow itself (#2529) was measured in a real browser.
describe("SshCard layout hooks (#2529)", () => {
  it("a key row keeps its fingerprint, comment and badge in one shrinkable column", () => {
    const row = host.querySelector("label.perm__row");
    const text = row?.querySelector(".ssh-card__keytext");
    expect(text?.querySelector("code")?.textContent).toBe(FP("a"));
    expect(text?.querySelector(".perm__detail")?.textContent).toBe("me@laptop");
    expect(text?.querySelector(".badge-sm")?.textContent).toBe("loaded");
  });

  it("a host key row with a compare tick is marked to wrap", async () => {
    const next = [...host.querySelectorAll("button")].find((b) => b.textContent === "Next");
    await act(async () => next?.click());
    const row = host.querySelector("li.perm__row");
    expect(row?.classList.contains("ssh-card__row--tick")).toBe(true);
    expect(row?.querySelector("label")?.textContent).toContain("I compared this");
    expect(row?.querySelector("code")?.textContent).toContain(FP("c"));
  });
});
