import React, { createContext, useContext, useEffect, useReducer } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AgentEntry } from "../types";

interface AgentContextValue {
  agents: AgentEntry[];
  selectedAgent: string | null;
  setSelected: (name: string | null) => void;
}

const AgentContext = createContext<AgentContextValue>({
  agents: [],
  selectedAgent: null,
  setSelected: () => {},
});

type Action =
  | { type: "set_agents"; agents: AgentEntry[] }
  | { type: "set_selected"; name: string | null };

interface State {
  agents: AgentEntry[];
  selectedAgent: string | null;
}

function reducer(state: State, action: Action): State {
  switch (action.type) {
    case "set_agents":
      return { ...state, agents: action.agents };
    case "set_selected":
      return { ...state, selectedAgent: action.name };
  }
}

export function AgentProvider({ children }: { children: React.ReactNode }) {
  const [state, dispatch] = useReducer(reducer, { agents: [], selectedAgent: null });

  useEffect(() => {
    invoke<AgentEntry[]>("list_agents")
      .then((agents) => dispatch({ type: "set_agents", agents }))
      .catch(console.error);

    const unlisten = listen<AgentEntry[]>("agents-updated", (event) => {
      dispatch({ type: "set_agents", agents: event.payload });
    });

    const unlistenSelect = listen<string>("select-agent", (event) => {
      dispatch({ type: "set_selected", name: event.payload });
    });

    return () => {
      unlisten.then((fn) => fn());
      unlistenSelect.then((fn) => fn());
    };
  }, []);

  return (
    <AgentContext.Provider
      value={{
        agents: state.agents,
        selectedAgent: state.selectedAgent,
        setSelected: (name) => dispatch({ type: "set_selected", name }),
      }}
    >
      {children}
    </AgentContext.Provider>
  );
}

export function useAgents() {
  return useContext(AgentContext);
}
