import { AgentProvider } from "./context/AgentContext";
import { ConversationProvider } from "./conversation/ConversationContext";
import { PopoverApp } from "./components/PopoverApp";
import { DashboardApp } from "./components/DashboardApp";
import { PetApp } from "./components/PetApp";

function getRoute(): "popover" | "dashboard" | "pet" {
  const hash = window.location.hash;
  if (hash === "#/popover") return "popover";
  if (hash.startsWith("#/pet/")) return "pet";
  return "dashboard";
}

export default function App() {
  const route = getRoute();
  if (route === "pet") return <PetApp />;
  return (
    <AgentProvider>
      <ConversationProvider>
        {route === "popover" ? <PopoverApp /> : <DashboardApp />}
      </ConversationProvider>
    </AgentProvider>
  );
}
