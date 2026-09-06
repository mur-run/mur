import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { NeedsYouBadge, StatusDot, fleetStatusOf, statusOf } from "./Status";

describe("statusOf", () => {
  it("maps every runtime state; stopped and unknown read as idle", () => {
    expect(statusOf({ state: "running", pid: 1 })).toBe("running");
    expect(statusOf({ state: "restarting", attempt: 1, backoff_secs: 2 })).toBe("restarting");
    expect(statusOf({ state: "failed" })).toBe("failed");
    expect(statusOf({ state: "stopped" })).toBe("idle");
    expect(statusOf(undefined)).toBe("idle");
  });
});

describe("fleetStatusOf", () => {
  it("kill-switch wins over running", () => {
    expect(fleetStatusOf({ stopped: true, running: true })).toBe("stopped");
    expect(fleetStatusOf({ stopped: false, running: true })).toBe("running");
    expect(fleetStatusOf({ stopped: false, running: false })).toBe("idle");
  });
});

describe("markup", () => {
  it("dot carries the kind as a modifier class", () => {
    expect(renderToStaticMarkup(<StatusDot kind="failed" />)).toContain("status-dot--failed");
  });
  it("badge renders nothing at zero and caps at 99+", () => {
    expect(renderToStaticMarkup(<NeedsYouBadge count={0} />)).toBe("");
    expect(renderToStaticMarkup(<NeedsYouBadge count={120} />)).toContain("99+");
    expect(renderToStaticMarkup(<NeedsYouBadge count={3} />)).toContain(">3<");
  });
});
