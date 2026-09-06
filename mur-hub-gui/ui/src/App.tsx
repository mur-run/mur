import { AgentProvider } from "./context/AgentContext";
import { ConversationProvider } from "./conversation/ConversationContext";
import { PopoverApp } from "./components/PopoverApp";
import { DashboardApp } from "./components/DashboardApp";
import { PetApp } from "./components/PetApp";
import { AgentChatWindow } from "./components/chat/AgentChatWindow";
import { PanelWindow } from "./components/panel/PanelWindow";
import { DetailWindow } from "./components/detail/window/DetailWindow";

function getRoute(): "popover" | "dashboard" | "pet" | "chat" | "panel" | "detail" {
  const hash = window.location.hash;
  if (hash === "#/popover") return "popover";
  if (hash.startsWith("#/pet/")) return "pet";
  if (hash.startsWith("#/chat/")) return "chat";
  if (hash.startsWith("#/detail/")) return "detail";
  if (hash.startsWith("#/panel")) return "panel";
  return "dashboard";
}

export default function App() {
  const route = getRoute();
  if (route === "pet") return <PetApp />;
  if (route === "chat") return <AgentChatWindow />;
  if (route === "detail") return <DetailWindow />; // brings its own AgentProvider
  if (route === "panel") return <PanelWindow />;
  return (
    <AgentProvider>
      <ConversationProvider>
        {route === "popover" ? <PopoverApp /> : <DashboardApp />}
      </ConversationProvider>
    </AgentProvider>
  );
}
