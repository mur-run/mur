import { AgentProvider } from "./context/AgentContext";
import { PopoverApp } from "./components/PopoverApp";
import { DashboardApp } from "./components/DashboardApp";

function getRoute(): "popover" | "dashboard" {
  return window.location.hash === "#/popover" ? "popover" : "dashboard";
}

export default function App() {
  const route = getRoute();
  return (
    <AgentProvider>
      {route === "popover" ? <PopoverApp /> : <DashboardApp />}
    </AgentProvider>
  );
}
