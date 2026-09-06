export type AttentionLevel = "none" | "unread" | "hitl";

export interface ConversationAttention {
  unread: boolean;
  hitl: boolean;
}

export interface ConversationState {
  open: string[];
  active: string | null;
  attention: Record<string, ConversationAttention>;
}

export type ConversationAction =
  | { type: "open"; agent: string }
  | { type: "close"; agent: string }
  | { type: "focus"; agent: string }
  | { type: "delta"; agent: string }
  | { type: "hitl_open"; agent: string }
  /** No conversation is being looked at: every open one counts attention again. */
  | { type: "blur" };

const CLEAR: ConversationAttention = { unread: false, hitl: false };

export function initialConversationState(): ConversationState {
  return { open: [], active: null, attention: {} };
}

export function attentionLevel(a: ConversationAttention): AttentionLevel {
  if (a.hitl) return "hitl";
  if (a.unread) return "unread";
  return "none";
}

export function conversationReducer(
  state: ConversationState,
  action: ConversationAction,
): ConversationState {
  switch (action.type) {
    case "open": {
      if (state.open.includes(action.agent)) {
        return { ...state, active: action.agent };
      }
      return {
        ...state,
        open: [...state.open, action.agent],
        active: action.agent,
        attention: { ...state.attention, [action.agent]: { ...CLEAR } },
      };
    }
    case "close": {
      const open = state.open.filter((a) => a !== action.agent);
      const attention = { ...state.attention };
      delete attention[action.agent];
      const active =
        state.active === action.agent
          ? (open[open.length - 1] ?? null)
          : state.active;
      return { open, active, attention };
    }
    case "focus": {
      if (!state.open.includes(action.agent)) return state;
      return {
        ...state,
        active: action.agent,
        attention: {
          ...state.attention,
          [action.agent]: { ...CLEAR },
        },
      };
    }
    case "delta": {
      if (!state.open.includes(action.agent) || state.active === action.agent)
        return state;
      const prev = state.attention[action.agent] ?? { ...CLEAR };
      return {
        ...state,
        attention: { ...state.attention, [action.agent]: { ...prev, unread: true } },
      };
    }
    case "blur": {
      return state.active === null ? state : { ...state, active: null };
    }
    case "hitl_open": {
      if (!state.open.includes(action.agent) || state.active === action.agent)
        return state;
      const prev = state.attention[action.agent] ?? { ...CLEAR };
      return {
        ...state,
        attention: { ...state.attention, [action.agent]: { ...prev, hitl: true } },
      };
    }
  }
}
