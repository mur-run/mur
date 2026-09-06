import { describe, it, expect } from "vitest";
import {
  conversationReducer,
  initialConversationState,
  attentionLevel,
  type ConversationState,
} from "./reducer";

function open(state: ConversationState, agent: string) {
  return conversationReducer(state, { type: "open", agent });
}

describe("conversationReducer", () => {
  it("open adds agent and sets active", () => {
    const s = open(initialConversationState(), "a");
    expect(s.open).toContain("a");
    expect(s.active).toBe("a");
  });

  it("open twice focuses without duplicating", () => {
    let s = open(initialConversationState(), "a");
    s = open(s, "b");
    s = open(s, "a");
    expect(s.open.filter((x) => x === "a")).toHaveLength(1);
    expect(s.active).toBe("a");
  });

  it("close removes agent and shifts active to last remaining", () => {
    let s = open(initialConversationState(), "a");
    s = open(s, "b");
    s = conversationReducer(s, { type: "close", agent: "b" });
    expect(s.open).not.toContain("b");
    expect(s.active).toBe("a");
  });

  it("delta for non-active open agent sets unread", () => {
    let s = open(initialConversationState(), "a");
    s = open(s, "b");
    s = conversationReducer(s, { type: "focus", agent: "b" });
    s = conversationReducer(s, { type: "delta", agent: "a" });
    expect(s.attention["a"].unread).toBe(true);
  });

  it("delta for the active agent does NOT set unread", () => {
    let s = open(initialConversationState(), "a");
    s = conversationReducer(s, { type: "delta", agent: "a" });
    expect(s.attention["a"].unread).toBe(false);
  });

  it("delta for an agent that is not open is ignored", () => {
    const s = conversationReducer(initialConversationState(), {
      type: "delta",
      agent: "ghost",
    });
    expect(s.attention["ghost"]).toBeUndefined();
  });

  it("hitl_open for a non-active open agent sets hitl", () => {
    let s = open(initialConversationState(), "a");
    s = open(s, "b");
    s = conversationReducer(s, { type: "focus", agent: "b" });
    s = conversationReducer(s, { type: "hitl_open", agent: "a" });
    expect(s.attention["a"].hitl).toBe(true);
  });

  it("focus clears unread and hitl for that agent", () => {
    let s = open(initialConversationState(), "a");
    s = open(s, "b");
    s = conversationReducer(s, { type: "focus", agent: "b" });
    s = conversationReducer(s, { type: "delta", agent: "a" });
    s = conversationReducer(s, { type: "hitl_open", agent: "a" });
    s = conversationReducer(s, { type: "focus", agent: "a" });
    expect(s.active).toBe("a");
    expect(s.attention["a"]).toEqual({ unread: false, hitl: false });
  });
});

describe("attentionLevel", () => {
  it("hitl wins over unread", () => {
    expect(attentionLevel({ unread: true, hitl: true })).toBe("hitl");
  });
  it("unread alone", () => {
    expect(attentionLevel({ unread: true, hitl: false })).toBe("unread");
  });
  it("none", () => {
    expect(attentionLevel({ unread: false, hitl: false })).toBe("none");
  });
});

describe("blur", () => {
  it("clears the active conversation so its deltas count as unread again", () => {
    let s = open(initialConversationState(), "a");
    s = conversationReducer(s, { type: "delta", agent: "a" });
    expect(s.attention.a.unread).toBe(false); // active: being looked at
    s = conversationReducer(s, { type: "blur" });
    expect(s.active).toBeNull();
    s = conversationReducer(s, { type: "delta", agent: "a" });
    expect(s.attention.a.unread).toBe(true);
    expect(conversationReducer(s, { type: "blur" })).toBe(s); // already blurred: same state
  });
});
