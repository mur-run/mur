import React, { createContext, useContext, useEffect, useReducer } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  conversationReducer,
  initialConversationState,
  type ConversationAttention,
} from "./reducer";

interface ConversationContextValue {
  open: string[];
  active: string | null;
  attention: Record<string, ConversationAttention>;
  openConversation: (agent: string) => void;
  closeConversation: (agent: string) => void;
  focusConversation: (agent: string) => void;
}

const Ctx = createContext<ConversationContextValue>({
  open: [],
  active: null,
  attention: {},
  openConversation: () => {},
  closeConversation: () => {},
  focusConversation: () => {},
});

export function ConversationProvider({ children }: { children: React.ReactNode }) {
  const [state, dispatch] = useReducer(
    conversationReducer,
    undefined,
    initialConversationState,
  );

  useEffect(() => {
    const unDelta = listen<{ agent: string }>("chat-delta", (e) => {
      dispatch({ type: "delta", agent: e.payload.agent });
    });
    const unHitl = listen<{ agent: string }>("hitl-approval-needed", (e) => {
      dispatch({ type: "hitl_open", agent: e.payload.agent });
    });
    return () => {
      unDelta.then((f) => f());
      unHitl.then((f) => f());
    };
  }, []);

  return (
    <Ctx.Provider
      value={{
        open: state.open,
        active: state.active,
        attention: state.attention,
        openConversation: (agent) => dispatch({ type: "open", agent }),
        closeConversation: (agent) => dispatch({ type: "close", agent }),
        focusConversation: (agent) => dispatch({ type: "focus", agent }),
      }}
    >
      {children}
    </Ctx.Provider>
  );
}

export function useConversations() {
  return useContext(Ctx);
}
