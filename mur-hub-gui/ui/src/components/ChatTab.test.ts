import { describe, it, expect } from "vitest";
import { newTaskId, buildStoppedMessage } from "./ChatTab";

describe("newTaskId", () => {
  it("produces a unique, task-prefixed id each call", () => {
    const a = newTaskId();
    const b = newTaskId();
    expect(a).toMatch(/^task-/);
    expect(a).not.toEqual(b);
  });
});

describe("buildStoppedMessage", () => {
  it("commits the partial buffer tagged stopped", () => {
    expect(buildStoppedMessage("partial answer")).toEqual({
      role: "agent",
      text: "partial answer",
      stopped: true,
    });
  });

  it("still returns a stopped marker when the buffer is empty", () => {
    expect(buildStoppedMessage("")).toEqual({
      role: "agent",
      text: "",
      stopped: true,
    });
  });
});
