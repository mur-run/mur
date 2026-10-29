import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { LanguageProvider } from "../../../i18n";
import { DangerZone, DangerZoneBody } from "./DangerZone";
import { OverrideDialog } from "./OverrideDialog";
import { OverrideBadge } from "../../shell/OverrideBadge";

// No jsdom in this test env; LanguageProvider reads localStorage on mount.
(globalThis as { localStorage?: Storage }).localStorage ??= {
  getItem: () => null,
  setItem: () => {},
  removeItem: () => {},
  clear: () => {},
  key: () => null,
  length: 0,
} as Storage;

const html = (el: React.ReactElement) => renderToStaticMarkup(<LanguageProvider>{el}</LanguageProvider>);
const noop = () => {};

describe("DangerZone", () => {
  it("renders nothing outside the dashboard", () => {
    expect(html(<DangerZone agentName="a" isRunning />)).toBe("");
    expect(html(<DangerZone agentName="a" isRunning dashboardOnly={false} />)).toBe("");
  });

  it("starts collapsed: header only", () => {
    const out = html(<DangerZone agentName="a" isRunning dashboardOnly />);
    expect(out).toContain("Danger zone");
    expect(out).toContain('aria-expanded="false"');
    expect(out).not.toContain("Restart with overrides");
    expect(out).not.toContain("for one run only");
  });

  it("body offers the button only while running", () => {
    const running = html(<DangerZoneBody agentName="a" isRunning onOpen={noop} />);
    expect(running).toContain("for one run only");
    expect(running).toContain("Restart with overrides");
    const stopped = html(<DangerZoneBody agentName="a" isRunning={false} onOpen={noop} />);
    expect(stopped).toContain("for one run only");
    expect(stopped).not.toContain("<button");
  });
});

describe("OverrideDialog", () => {
  it("starts with four unchecked boxes and Continue disabled", () => {
    const out = html(<OverrideDialog agentName="a" onClose={noop} />);
    expect((out.match(/type="checkbox"/g) ?? []).length).toBe(4);
    expect(out).not.toMatch(/checked=""/);
    expect(out).toMatch(/<button[^>]*disabled=""[^>]*>Continue<\/button>/);
  });
});

describe("OverrideBadge", () => {
  it("is passive without an action", () => {
    const out = html(<OverrideBadge flags={["--no-hitl", "--full-perms"]} />);
    expect(out).toContain("--no-hitl --full-perms");
    expect(out).not.toContain("<button");
  });

  it("offers Restart normally when given an action", () => {
    const out = html(<OverrideBadge flags={["--no-hitl"]} onRestartNormally={noop} />);
    expect((out.match(/<button/g) ?? []).length).toBe(1);
    expect(out).toContain("Restart normally");
  });
});
