import { describe, it, expect } from "vitest";
import { canSubmitMode, buildParallelPayload, DURATION_RE } from "./fleetCreateForm";

describe("canSubmitMode", () => {
  it("plain mode always submittable", () => {
    expect(canSubmitMode("plain", [], "", "")).toBe(true);
  });
  it("speculative needs >=2 tracks and a judge model", () => {
    expect(canSubmitMode("speculative", [], "claude-opus-4-8", "")).toBe(false);
    expect(canSubmitMode("speculative", [{ name: "a", approach: "", model: "" }], "claude-opus-4-8", "")).toBe(false);
    const twoTracks = [
      { name: "a", approach: "", model: "" },
      { name: "b", approach: "", model: "" },
    ];
    expect(canSubmitMode("speculative", twoTracks, "", "")).toBe(false); // missing judge model
    expect(canSubmitMode("speculative", twoTracks, "claude-opus-4-8", "")).toBe(true);
  });
  it("partition needs a target file and a judge model", () => {
    expect(canSubmitMode("partition", [], "claude-opus-4-8", "")).toBe(false);
    expect(canSubmitMode("partition", [], "", "src/widget.rs")).toBe(false);
    expect(canSubmitMode("partition", [], "claude-opus-4-8", "src/widget.rs")).toBe(true);
  });
});

describe("buildParallelPayload", () => {
  it("plain mode returns null", () => {
    expect(buildParallelPayload("plain", [], "", "", false, false)).toBeNull();
  });
  it("speculative builds tracks + judge + pre_filter", () => {
    const tracks = [
      { name: "track-a", approach: "functional style", model: "" },
      { name: "track-b", approach: "performance first", model: "claude-opus-4-8" },
    ];
    const payload = buildParallelPayload("speculative", tracks, "claude-opus-4-8", "", true, false);
    expect(payload).toEqual({
      mode: "speculative",
      tracks: [
        { name: "track-a", approach: "functional style", model: null },
        { name: "track-b", approach: "performance first", model: "claude-opus-4-8" },
      ],
      judge: { model: "claude-opus-4-8" },
      pre_filter: ["cargo_check"],
    });
  });
  it("partition builds target_file, empty tracks", () => {
    const payload = buildParallelPayload("partition", [], "claude-opus-4-8", "src/widget.rs", false, false);
    expect(payload).toEqual({
      mode: "partition",
      tracks: [],
      judge: { model: "claude-opus-4-8" },
      pre_filter: [],
      partition: { target_file: "src/widget.rs" },
    });
  });
});

describe("DURATION_RE", () => {
  it("accepts mur-core's parse_duration formats", () => {
    for (const v of ["30s", "5m", "2h", "1d", "8"]) {
      expect(DURATION_RE.test(v)).toBe(true);
    }
  });
  it("rejects calendar dates and unsupported units", () => {
    for (const v of ["2026-12-31", "1w", "", "abc"]) {
      expect(DURATION_RE.test(v)).toBe(false);
    }
  });
});
