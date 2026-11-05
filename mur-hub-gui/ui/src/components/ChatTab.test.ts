import { describe, it, expect } from "vitest";
import { newTaskId, buildStoppedMessage, channelEventsToMessages } from "./ChatTab";

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

describe("channelEventsToMessages", () => {
  const base = { ts: "2026-01-01T00:00:00Z", idempotency_key: null };
  it("renders consults as muted rows, never as the agent's reply", () => {
    const msgs = channelEventsToMessages([
      { ...base, seq: 0, actor: { kind: "agent", id: "pm" }, kind: "ask", payload: { to: "tech-writer", question: "which file?" } },
      { ...base, seq: 1, actor: { kind: "agent", id: "tech-writer" }, kind: "ask-reply", payload: { text: "README.md" } },
      { ...base, seq: 2, actor: { kind: "agent", id: "pm" }, kind: "message", payload: { text: "done" } },
      { ...base, seq: 3, actor: { kind: "agent", id: "pm" }, kind: "note", payload: { text: "hidden" } },
    ]);
    expect(msgs).toEqual([
      { role: "consult", text: "pm → tech-writer: which file?" },
      { role: "consult", text: "tech-writer ↩ README.md" },
      { role: "agent", text: "done" },
    ]);
  });
});
