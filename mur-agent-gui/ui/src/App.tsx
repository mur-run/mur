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
import { OnboardingWizard } from "./onboarding/OnboardingWizard";
import {
  setTheme as setThemeApi,
  getDefaultTheme,
  applyThemeColors,
  getOnboardingStatus,
  status as getStatus,
} from "./lib/api";

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

  // First-launch onboarding gate. `null` while the status round-trip
  // is in flight (UI renders normally), `false` once we've confirmed
  // the agent has never completed onboarding (wizard overlays), `true`
  // once it has (or once a fetch error swallows the question — we
  // never block the UI on a status hiccup). The agent name comes from
  // the existing `status` command, which reads `MUR_GUI_AGENT_NAME`
  // baked in by the bundle export pipeline; falling back to `default`
  // keeps a hand-built dev shell working too.
  const [onboarded, setOnboarded] = useState<boolean | null>(null);
  const [agent, setAgent] = useState<string>("default");

  // Apply the bundle's baked-in default theme on mount, so the
  // window picks up the colors chosen at `mur agent export --theme`.
  useEffect(() => {
    getDefaultTheme()
      .then((t) => applyThemeColors(t.colors))
      .catch(() => {});
  }, []);

  // Resolve the bundled agent name and probe its onboarding state.
  // Both calls are tolerant of failure: a missing agent or a transient
  // IPC error treats the user as already-onboarded so the wizard
  // never traps them on a broken install.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      let name = "default";
      try {
        const s = await getStatus();
        if (s?.name) name = s.name;
      } catch {
        // Keep the fallback agent name; the onboarding probe below
        // will also tolerate failure and let the UI through.
      }
      if (cancelled) return;
      setAgent(name);
      try {
        const o = await getOnboardingStatus(name);
        if (!cancelled) setOnboarded(!!o.completed_at);
      } catch {
        if (!cancelled) setOnboarded(true);
      }
    })();
    return () => {
      cancelled = true;
    };
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
      {onboarded === false && (
        <OnboardingWizard
          agent={agent}
          onComplete={() => setOnboarded(true)}
          onOpenVoiceSettings={() => setTab("voice")}
        />
      )}
    </div>
  );
}
