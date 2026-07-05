import React, { createContext, useContext, useEffect, useReducer } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AgentEntry, AgentRuntimeStatus } from "../types";

interface AgentContextValue {
  agents: AgentEntry[];
  runtimeStatuses: AgentRuntimeStatus[];
  selectedAgent: string | null;
  setSelected: (name: string | null) => void;
  /** When set, the AgentInspector opens focused on this tab (consumed once). */
  desiredDetailTab: string | null;
  setDesiredDetailTab: (tab: string | null) => void;
}

const AgentContext = createContext<AgentContextValue>({
  agents: [],
  runtimeStatuses: [],
  selectedAgent: null,
  setSelected: () => {},
  desiredDetailTab: null,
  setDesiredDetailTab: () => {},
});

type Action =
  | { type: "set_agents"; agents: AgentEntry[] }
  | { type: "set_runtime"; statuses: AgentRuntimeStatus[] }
  | { type: "set_selected"; name: string | null }
  | { type: "set_desired_tab"; tab: string | null };

interface State {
  agents: AgentEntry[];
  runtimeStatuses: AgentRuntimeStatus[];
  selectedAgent: string | null;
  desiredDetailTab: string | null;
}

function reducer(state: State, action: Action): State {
  switch (action.type) {
    case "set_agents":
      return { ...state, agents: action.agents };
    case "set_runtime":
      return { ...state, runtimeStatuses: action.statuses };
    case "set_selected":
      return { ...state, selectedAgent: action.name };
    case "set_desired_tab":
      return { ...state, desiredDetailTab: action.tab };
  }
}

export function AgentProvider({ children }: { children: React.ReactNode }) {
  const [state, dispatch] = useReducer(reducer, {
    agents: [],
    runtimeStatuses: [],
    selectedAgent: null,
    desiredDetailTab: null,
  });

  useEffect(() => {
    invoke<AgentEntry[]>("list_agents")
      .then((agents) => dispatch({ type: "set_agents", agents }))
      .catch(console.error);

    const unAgents = listen<AgentEntry[]>("agents-updated", (e) =>
      dispatch({ type: "set_agents", agents: e.payload }),
    );
    const unRuntime = listen<AgentRuntimeStatus[]>("runtime-status-changed", (e) =>
      dispatch({ type: "set_runtime", statuses: e.payload }),
    );
    const unSelect = listen<string>("select-agent", (e) =>
      dispatch({ type: "set_selected", name: e.payload }),
    );

    return () => {
      unAgents.then((fn) => fn());
      unRuntime.then((fn) => fn());
      unSelect.then((fn) => fn());
    };
  }, []);

  return (
    <AgentContext.Provider
      value={{
        agents: state.agents,
        runtimeStatuses: state.runtimeStatuses,
        selectedAgent: state.selectedAgent,
        setSelected: (name) => dispatch({ type: "set_selected", name }),
        desiredDetailTab: state.desiredDetailTab,
        setDesiredDetailTab: (tab) => dispatch({ type: "set_desired_tab", tab }),
      }}
    >
      {children}
    </AgentContext.Provider>
  );
}

export function useAgents() {
  return useContext(AgentContext);
}
