import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useAgents } from "../../context/AgentContext";
import type { AgentDetail } from "../../types";
import { useT } from "../../i18n";
import { AgentPicker } from "./AgentPicker";

// ─── Backend shape (snake_case, from `addons_installed`) ─────────────────────

interface AddonAgentState {
  agent: string;
  enabled: boolean;
}

interface InstalledAddonAgg {
  id: string;
  source: string;
  skill_count: number;
  mcp_count: number;
  command_count: number;
  agents: AddonAgentState[];
}

// ─── Component ────────────────────────────────────────────────────────────────

export function PluginsPage() {
  const { t } = useT();
  const { agents } = useAgents();
  const [addons, setAddons] = useState<InstalledAddonAgg[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [targetAgent, setTargetAgent] = useState<string>(agents[0]?.name ?? "");

  useEffect(() => {
    if (!targetAgent && agents[0]?.name) {
      setTargetAgent(agents[0].name);
    }
  }, [agents, targetAgent]);

  const refresh = useCallback(() => {
    setLoading(true);
    invoke<InstalledAddonAgg[]>("addons_installed")
      .then((res) => {
        setAddons(res);
        setError(null);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  async function importPlugin() {
    if (!targetAgent) return;
    setError(null);
    setBusy(true);
    try {
      const dir = await open({ directory: true, title: "Select a Claude plugin folder" });
      if (!dir || Array.isArray(dir)) return;
      await invoke<AgentDetail>("agent_addon_import", {
        name: targetAgent,
        pluginDir: dir,
        force: false,
      });
      refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function toggleAddon(agentName: string, addonId: string, enabled: boolean) {
    setError(null);
    setBusy(true);
    try {
      await invoke<AgentDetail>("agent_addon_toggle", {
        name: agentName,
        addonId,
        enabled,
      });
      refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function removeAddon(agentName: string, addonId: string) {
    setError(null);
    setBusy(true);
    try {
      await invoke<AgentDetail>("agent_addon_remove", {
        name: agentName,
        addonId,
      });
      refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div>
      <div
        className="tab-form"
        style={{ display: "flex", gap: 8, alignItems: "flex-start", marginBottom: 12 }}
      >
        <AgentPicker agents={agents} value={targetAgent} onChange={setTargetAgent} />
        <button
          className="btn btn--sm btn--secondary"
          onClick={importPlugin}
          disabled={!targetAgent || busy}
        >
          {t("pluginslib.import")}
        </button>
      </div>

      {error && <p className="save-error">{error}</p>}

      {loading ? (
        <p className="field-muted">{t("pluginslib.loading")}</p>
      ) : addons.length === 0 ? (
        <div className="tab-empty">
          <p>{t("pluginslib.empty")}</p>
        </div>
      ) : (
        <ul className="item-list">
          {addons.map((a) => (
            <li key={a.id} className="item-card">
              <div
                className="item-card-name"
                style={{ display: "flex", gap: 6, alignItems: "center" }}
              >
                <span style={{ fontWeight: 600 }}>{a.id}</span>
                <span className="item-card__meta">{a.source}</span>
                <span className="item-card__meta">
                  {a.skill_count} {t("pluginslib.skills")} / {a.mcp_count}{" "}
                  {t("pluginslib.mcp")} / {a.command_count} {t("pluginslib.commands")}
                </span>
              </div>
              <div style={{ marginTop: 8, display: "flex", flexDirection: "column", gap: 4 }}>
                <span className="field-muted" style={{ fontSize: 12 }}>
                  {t("pluginslib.usedBy")}
                </span>
                {a.agents.map((st) => (
                  <div
                    key={st.agent}
                    style={{ display: "flex", gap: 8, alignItems: "center" }}
                  >
                    <label style={{ display: "flex", gap: 6, alignItems: "center", fontSize: 13 }}>
                      <input
                        type="checkbox"
                        checked={st.enabled}
                        disabled={busy}
                        onChange={(e) => toggleAddon(st.agent, a.id, e.target.checked)}
                      />
                      <span>{st.agent}</span>
                    </label>
                    <button
                      className="btn btn--xs btn--ghost"
                      disabled={busy}
                      onClick={() => removeAddon(st.agent, a.id)}
                      title={t("pluginslib.remove")}
                    >
                      {t("pluginslib.remove")}
                    </button>
                  </div>
                ))}
              </div>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
