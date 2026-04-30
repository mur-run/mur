import { useEffect, useState } from "react";
import StatusTab from "./tabs/Status";
import PromptTab from "./tabs/Prompt";
import ModelTab from "./tabs/Model";
import SkillsTab from "./tabs/Skills";
import McpTab from "./tabs/Mcp";
import PermissionsTab from "./tabs/Permissions";
import IdentityTab from "./tabs/Identity";
import { VoiceTab } from "./voice/VoiceTab";
import { PttButton } from "./voice/PttButton";
import { setTheme as setThemeApi, getDefaultTheme, applyThemeColors } from "./lib/api";

type TabId =
  | "status"
  | "prompt"
  | "model"
  | "voice"
  | "skills"
  | "mcp"
  | "permissions"
  | "identity";

const TABS: { id: TabId; label: string }[] = [
  { id: "status", label: "Status" },
  { id: "prompt", label: "System Prompt" },
  { id: "model", label: "Model" },
  { id: "voice", label: "Voice" },
  { id: "skills", label: "Skills" },
  { id: "mcp", label: "MCP Servers" },
  { id: "permissions", label: "Permissions" },
  { id: "identity", label: "Identity" },
];

export default function App() {
  const [tab, setTab] = useState<TabId>("status");

  // Apply the bundle's baked-in default theme on mount, so the
  // window picks up the colors chosen at `mur agent export --theme`.
  useEffect(() => {
    getDefaultTheme()
      .then((t) => applyThemeColors(t.colors))
      .catch(() => {});
  }, []);

  // OS appearance subscriber for the "Match System" mode (spec § 7.3).
  // When the user has selected the synthetic "system" theme, follow
  // prefers-color-scheme — applying "light" or "dark" via the Tauri
  // command. The actual stored preference (system vs explicit theme)
  // is persisted by tauri-plugin-store; this hook only acts when the
  // localStorage key indicates system-following mode. Caches the last
  // applied target in a ref so we don't fire a Tauri IPC on every
  // matchMedia event when the OS toggles back and forth without a
  // semantic change.
  useEffect(() => {
    const matcher = window.matchMedia("(prefers-color-scheme: dark)");
    let lastApplied: string | null = null;
    const apply = () => {
      const pref = localStorage.getItem("mur.theme.mode");
      if (pref !== "system") return;
      const target = matcher.matches ? "dark" : "light";
      if (target === lastApplied) return;
      lastApplied = target;
      setThemeApi(target)
        .then((t) => applyThemeColors(t.colors))
        .catch(() => {});
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
        {tab === "model" && <ModelTab />}
        {tab === "voice" && <VoiceTab />}
        {tab === "skills" && <SkillsTab />}
        {tab === "mcp" && <McpTab />}
        {tab === "permissions" && <PermissionsTab />}
        {tab === "identity" && <IdentityTab />}
      </main>
      <PttButton
        onTranscript={(text) => {
          // For now, log + console. Future slices will wire this into
          // an active chat composer. The transcript itself is already
          // visible to the user via the recording → transcribing flow.
          // eslint-disable-next-line no-console
          console.log("ptt transcript:", text);
        }}
      />
    </div>
  );
}
