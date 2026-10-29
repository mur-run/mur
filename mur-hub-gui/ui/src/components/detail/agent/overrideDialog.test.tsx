// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { LanguageProvider } from "../../../i18n";

// Restart never settles: the dialog stays in its busy state for the test.
const invoke = vi.fn((cmd: string) =>
  cmd === "override_banner" ? Promise.resolve("BANNER") : new Promise(() => {}),
);
vi.mock("@tauri-apps/api/core", () => ({ invoke: (cmd: string) => invoke(cmd) }));

const { OverrideDialog } = await import("./OverrideDialog");

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
// jsdom here has no usable localStorage; LanguageProvider reads it on mount.
Object.defineProperty(globalThis, "localStorage", {
  configurable: true,
  value: { getItem: () => null, setItem: () => {}, removeItem: () => {}, clear: () => {}, key: () => null, length: 0 },
});

let host: HTMLElement;
let root: Root;
let opener: HTMLButtonElement;
let background: HTMLButtonElement;

function tab(shift = false) {
  const ev = new KeyboardEvent("keydown", { key: "Tab", shiftKey: shift, bubbles: true, cancelable: true });
  (document.activeElement ?? document.body).dispatchEvent(ev);
  return ev;
}

async function render(onClose = () => {}) {
  await act(async () => {
    root.render(
      <LanguageProvider>
        <OverrideDialog agentName="alpha" onClose={onClose} />
      </LanguageProvider>,
    );
  });
}

beforeEach(() => {
  host = document.createElement("div");
  host.id = "root";
  background = document.createElement("button");
  background.textContent = "Other tab";
  opener = document.createElement("button");
  opener.textContent = "Restart with overrides";
  const mount = document.createElement("div");
  host.append(background, opener, mount);
  document.body.append(host);
  opener.focus();
  root = createRoot(mount);
});

afterEach(() => {
  act(() => root.unmount());
  document.body.innerHTML = "";
  invoke.mockClear();
});

const dialog = () => document.querySelector<HTMLElement>('[role="dialog"]')!;

describe("OverrideDialog initial state", () => {
  it("starts with four unchecked boxes and Continue disabled", async () => {
    await render();
    const boxes = dialog().querySelectorAll<HTMLInputElement>('input[type="checkbox"]');
    expect(boxes.length).toBe(4);
    expect(Array.from(boxes).some((b) => b.checked)).toBe(false);
    const go = dialog().querySelector<HTMLButtonElement>(".btn--danger")!;
    expect(go.textContent).toBe("Continue");
    expect(go.disabled).toBe(true);
  });
});

describe("OverrideDialog focus management", () => {
  it("is a labelled modal dialog outside the app root", async () => {
    await render();
    const d = dialog();
    expect(d).not.toBeNull();
    expect(d.getAttribute("aria-modal")).toBe("true");
    const title = document.getElementById(d.getAttribute("aria-labelledby")!);
    expect(title?.textContent).toContain("alpha");
    expect(host.contains(d)).toBe(false);
  });

  it("moves focus inside and makes the background inert", async () => {
    await render();
    expect(dialog().contains(document.activeElement)).toBe(true);
    expect(host.hasAttribute("inert")).toBe(true);
  });

  it("keeps Tab inside the dialog while the restart is pending", async () => {
    await render();
    const d = dialog();
    const [a, b, c] = Array.from(d.querySelectorAll<HTMLInputElement>('input[type="checkbox"]'));
    await act(async () => { a.click(); b.click(); c.click(); });
    const name = d.querySelector<HTMLInputElement>("#override-confirm-name")!;
    await act(async () => {
      const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
      set.call(name, "alpha");
      name.dispatchEvent(new Event("input", { bubbles: true }));
    });
    const go = d.querySelector<HTMLButtonElement>(".btn--danger")!;
    expect(go.disabled).toBe(false);
    await act(async () => { go.click(); });
    expect(invoke).toHaveBeenCalledWith("restart_agent_with_overrides");
    expect(d.textContent).toContain("alpha");

    // Every control is disabled while busy; focus must still not escape.
    for (let i = 0; i < 4; i++) {
      tab();
      tab(true);
    }
    expect(d.contains(document.activeElement)).toBe(true);
    expect(document.activeElement).not.toBe(background);
    expect(host.hasAttribute("inert")).toBe(true);
  });

  it("wraps Tab from the last control to the first and back", async () => {
    await render();
    const d = dialog();
    const focusable = Array.from(
      d.querySelectorAll<HTMLElement>("button:not([disabled]), input:not([disabled])"),
    );
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    last.focus();
    expect(tab().defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(first);
    first.focus();
    tab(true);
    expect(document.activeElement).toBe(last);
  });

  it("restores the background and focus on close", async () => {
    await render();
    await act(async () => root.render(<></>));
    expect(host.hasAttribute("inert")).toBe(false);
    expect(document.activeElement).toBe(opener);
  });
});
