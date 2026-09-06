import React, { createContext, useCallback, useContext, useEffect, useReducer, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  conversationReducer,
  initialConversationState,
  type ConversationAttention,
} from "./reducer";
import { notifyHitlBackground } from "./notify";

interface ConversationContextValue {
  open: string[];
  active: string | null;
  attention: Record<string, ConversationAttention>;
  openConversation: (agent: string) => void;
  closeConversation: (agent: string) => void;
  focusConversation: (agent: string) => void;
  /** Nothing is being looked at (the Chats page cleared its selection). */
  blurConversation: () => void;
  /** Pending chat-input text per agent (e.g. staged by file-drop on a pet). */
  drafts: Record<string, string>;
  setDraft: (agent: string, text: string) => void;
  clearDraft: (agent: string) => void;
}

const Ctx = createContext<ConversationContextValue>({
  open: [],
  active: null,
  attention: {},
  openConversation: () => {},
  closeConversation: () => {},
  focusConversation: () => {},
  blurConversation: () => {},
  drafts: {},
  setDraft: () => {},
  clearDraft: () => {},
});

export function ConversationProvider({ children }: { children: React.ReactNode }) {
  const [state, dispatch] = useReducer(
    conversationReducer,
    undefined,
    initialConversationState,
  );
  const stateRef = useRef(state);
  stateRef.current = state;
  const [drafts, setDrafts] = useState<Record<string, string>>({});

  useEffect(() => {
    const unDelta = listen<{ agent: string }>("chat-delta", (e) => {
      dispatch({ type: "delta", agent: e.payload.agent });
    });
    const unHitl = listen<{ agent: string; display_name?: string }>(
      "hitl-approval-needed",
      (e) => {
        const { agent, display_name } = e.payload;
        // stateRef lets us read current state inside the effect closure without
        // re-subscribing the listener on every render.
        const { open, active } = stateRef.current;
        if (open.includes(agent) && active !== agent) {
          notifyHitlBackground(display_name ?? agent);
        }
        dispatch({ type: "hitl_open", agent });
      },
    );
    return () => {
      unDelta.then((f) => f());
      unHitl.then((f) => f());
    };
  }, []);

  // Stable identities: the Chats page lists these as effect dependencies.
  const openConversation = useCallback((agent: string) => dispatch({ type: "open", agent }), []);
  const closeConversation = useCallback((agent: string) => dispatch({ type: "close", agent }), []);
  const focusConversation = useCallback((agent: string) => dispatch({ type: "focus", agent }), []);
  const blurConversation = useCallback(() => dispatch({ type: "blur" }), []);

  return (
    <Ctx.Provider
      value={{
        open: state.open,
        active: state.active,
        attention: state.attention,
        openConversation,
        closeConversation,
        focusConversation,
        blurConversation,
        drafts,
        setDraft: (agent, text) => setDrafts((d) => ({ ...d, [agent]: text })),
        clearDraft: (agent) =>
          setDrafts((d) => {
            if (!(agent in d)) return d;
            const next = { ...d };
            delete next[agent];
            return next;
          }),
      }}
    >
      {children}
    </Ctx.Provider>
  );
}

export function useConversations() {
  return useContext(Ctx);
}
