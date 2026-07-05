import { describe, expect, it } from "vitest";
import { statusBadgeClass } from "./SkillsPage";

describe("statusBadgeClass", () => {
  it("maps modified to a warn badge", () => {
    expect(statusBadgeClass("modified")).toBe("badge badge--warn");
  });

  it("maps update available to a warn badge", () => {
    expect(statusBadgeClass("update available")).toBe("badge badge--warn");
  });

  it("maps up to date to an ok badge", () => {
    expect(statusBadgeClass("up to date")).toBe("badge badge--ok");
  });

  it("maps unknown/dash status to the plain badge", () => {
    expect(statusBadgeClass("—")).toBe("badge");
  });
});
