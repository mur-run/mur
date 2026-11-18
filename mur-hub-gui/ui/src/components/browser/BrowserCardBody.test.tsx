// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { LanguageProvider } from "../../i18n";
import { BrowserCardBody } from "./BrowserCardBody";
import type { BodyView, CardView, OfferView, PickArg, PortStepView } from "./browserApi";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
// jsdom here has no usable localStorage; LanguageProvider reads it on mount.
Object.defineProperty(globalThis, "localStorage", {
  configurable: true,
  value: { getItem: () => null, setItem: () => {}, removeItem: () => {}, clear: () => {}, key: () => null, length: 0 },
});

let host: HTMLElement;
let root: Root;

beforeEach(() => {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

function card(body: BodyView, port_step: PortStepView | null = null): CardView {
  return { id: "c1", agent: "qa", site: "https://git.example", login: true, reason: "r", body, port_step };
}

async function render(c: CardView, onAnswer: (p: PickArg) => void) {
  await act(async () => {
    root.render(
      <LanguageProvider>
        <BrowserCardBody
          card={c}
          busy={false}
          loggingIn={false}
          result={null}
          note={null}
          onAnswer={onAnswer}
          onLoggedIn={() => {}}
        />
      </LanguageProvider>,
    );
  });
}

function button(label: string): HTMLButtonElement {
  const b = [...host.querySelectorAll("button")].find((x) => x.textContent?.startsWith(label));
  if (!b) throw new Error(`no ${label} button in: ${host.textContent}`);
  return b;
}

async function click(b: HTMLButtonElement) {
  await act(async () => b.click());
}

const login = (exists: boolean, login_offer: OfferView = { kind: "already_bound" }): BodyView => ({
  kind: "login",
  rows: [
    {
      name: "work",
      domains: ["git.example"],
      shared_with: [],
      offer: { kind: "already_bound" },
      session_elsewhere: null,
    },
  ],
  login_again: "work",
  login_again_exists: exists,
  login_offer,
});

describe("BrowserCardBody", () => {
  it("logs in again as a reauth, never as a binding replace", async () => {
    const onAnswer = vi.fn();
    await render(card(login(true)), onAnswer);
    await click(button("Log in again")); // first click asks to confirm the overwrite
    await click(button("Confirm replace"));
    expect(onAnswer).toHaveBeenCalledWith({ kind: "login", reauth: true, replace: false });
  });

  it("a replace_only login asks first, then answers replace without reauth (#2461)", async () => {
    const onAnswer = vi.fn();
    const offer: OfferView = { kind: "replace_only", with: "work", drops: ["old.example"], adds: [] };
    await render(card(login(false, offer)), onAnswer);
    await click(button("Log in again"));
    expect(onAnswer).not.toHaveBeenCalled();
    await click(button("Confirm replace"));
    expect(onAnswer).toHaveBeenCalledWith({ kind: "login", reauth: false, replace: true });
  });

  it("an already-bound row can still be used to finish the request", async () => {
    const onAnswer = vi.fn();
    await render(card(login(false)), onAnswer);
    await click(button("Use"));
    expect(onAnswer).toHaveBeenCalledWith({ kind: "row", name: "work", replace: false });
  });

  for (const reach of [{ kind: "allowed" }, { kind: "unscoped", mode: "inherit" }] as const) {
    it(`a ${reach.kind} reach can continue`, async () => {
      const onAnswer = vi.fn();
      await render(card({ kind: "reach", reach }), onAnswer);
      await click(button("Continue"));
      expect(onAnswer).toHaveBeenCalledWith({ kind: "allow" });
    });
  }

  it("copies a port command without the warning prose", async () => {
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    const cmd = "mur browser answer qa https://git.example:8443 --profile work --open-port";
    await render(
      card(login(false), { port: 8443, warning: "port 8443 is not open. ANY host", commands: [cmd] }),
      () => {},
    );
    expect(host.textContent).toContain("ANY host");
    await click(button("Copy"));
    expect(writeText).toHaveBeenCalledWith(cmd);
  });
});
