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

  it("names a non-agent consult actor `?`, matching the Rust renderer", () => {
    const msgs = channelEventsToMessages([
      { ...base, seq: 0, actor: { kind: "human", name: "david" }, kind: "ask", payload: { to: "pm", question: "status?" } },
    ]);
    expect(msgs).toEqual([{ role: "consult", text: "? → pm: status?" }]);
  });

  describe("retract", () => {
    const me = { kind: "human" as const, name: "local" };
    const human = (seq: number, text: string, actor: typeof me | { kind: "human"; name: string } = me) =>
      ({ ...base, seq, actor, kind: "message", payload: { text } });
    const agent = (seq: number, text: string) =>
      ({ ...base, seq, actor: { kind: "agent" as const, id: "mur" }, kind: "message", payload: { text } });
    const retract = (seq: number, target: number, actor: { kind: "human"; name: string } | { kind: "agent"; id: string } = me) =>
      ({ ...base, seq, actor, kind: "retract", payload: { target_seq: target, reason: "cancelled" } });

    it("hides a validly withdrawn message and the retract line", () => {
      const msgs = channelEventsToMessages([
        human(0, "hi"), agent(1, "hello"), human(2, "foo"), retract(3, 2), human(4, "foo bar"),
      ]);
      expect(msgs.map((m) => m.text)).toEqual(["hi", "hello", "foo bar"]);
    });

    it("keeps a message withdrawn by another actor", () => {
      const msgs = channelEventsToMessages([human(0, "foo"), retract(1, 0, { kind: "human", name: "eve" })]);
      expect(msgs.map((m) => m.text)).toEqual(["foo"]);
    });

    it("keeps an agent message", () => {
      const msgs = channelEventsToMessages([agent(0, "hello"), retract(1, 0, { kind: "agent", id: "mur" })]);
      expect(msgs.map((m) => m.text)).toEqual(["hello"]);
    });

    it("keeps a message that was already answered", () => {
      const msgs = channelEventsToMessages([human(0, "foo"), agent(1, "bar"), retract(2, 0)]);
      expect(msgs.map((m) => m.text)).toEqual(["foo", "bar"]);
    });

    it("ignores a forward or self target", () => {
      const msgs = channelEventsToMessages([human(0, "a"), retract(1, 2), retract(2, 2), human(3, "b")]);
      expect(msgs.map((m) => m.text)).toEqual(["a", "b"]);
    });
  });
});
