import { useEffect, useState } from "react";
import StatusTab from "./tabs/Status";
import PromptTab from "./tabs/Prompt";
import SkillsTab from "./tabs/Skills";
import McpTab from "./tabs/Mcp";
import PermissionsTab from "./tabs/Permissions";
import IdentityTab from "./tabs/Identity";
import { setTheme as setThemeApi } from "./lib/api";

type TabId =
  | "status"
  | "prompt"
  | "skills"
  | "mcp"
  | "permissions"
  | "identity";

const TABS: { id: TabId; label: string }[] = [
  { id: "status", label: "Status" },
  { id: "prompt", label: "System Prompt" },
  { id: "skills", label: "Skills" },
  { id: "mcp", label: "MCP Servers" },
  { id: "permissions", label: "Permissions" },
  { id: "identity", label: "Identity" },
];

export default function App() {
  const [tab, setTab] = useState<TabId>("status");

  // OS appearance subscriber for the "Match System" mode (spec § 7.3).
  // When the user has selected the synthetic "system" theme, follow
  // prefers-color-scheme — applying "light" or "dark" via the Tauri
  // command. The actual stored preference (system vs explicit theme)
  // is persisted by tauri-plugin-store; this hook only acts when the
  // localStorage key indicates system-following mode.
  useEffect(() => {
    const matcher = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      const pref = localStorage.getItem("mur.theme.mode");
      if (pref === "system") {
        const target = matcher.matches ? "dark" : "light";
        setThemeApi(target).catch(() => {});
      }
    };
    apply();
    matcher.addEventListener("change", apply);
    return () => matcher.removeEventListener("change", apply);
  }, []);
  return (
    <div className="flex h-full">
      <nav
        className="w-44 border-r"
        style={{ borderColor: "var(--color-border)", background: "var(--color-bg-secondary)" }}
      >
        <ul className="py-2">
          {TABS.map((t) => (
            <li key={t.id}>
              <button
                className="w-full px-3 py-2 text-left text-sm"
                style={{
                  background: tab === t.id ? "var(--color-accent)" : "transparent",
                  color: tab === t.id ? "var(--color-accent-fg)" : "var(--color-fg)",
                }}
                onClick={() => setTab(t.id)}
              >
                {t.label}
              </button>
            </li>
          ))}
        </ul>
      </nav>
      <main className="flex-1 overflow-auto p-6">
        {tab === "status" && <StatusTab />}
        {tab === "prompt" && <PromptTab />}
        {tab === "skills" && <SkillsTab />}
        {tab === "mcp" && <McpTab />}
        {tab === "permissions" && <PermissionsTab />}
        {tab === "identity" && <IdentityTab />}
      </main>
    </div>
  );
}
