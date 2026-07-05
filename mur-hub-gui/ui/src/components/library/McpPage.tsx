import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAgents } from "../../context/AgentContext";
import type { AgentDetail } from "../../types";
import { useT } from "../../i18n";
import { McpDiscoverModal } from "../McpDiscoverModal";
import { McpAddRemoteModal } from "../McpAddRemoteModal";
import { AgentPicker } from "./AgentPicker";

// ─── Backend shape ───────────────────────────────────────────────────────────

interface InstalledMcpView {
  id: string;
  name: string;
  description: string;
  transport: string;
  agents: string[];
}

// ─── Component ────────────────────────────────────────────────────────────────

export function McpPage() {
  const { t } = useT();
  const { agents } = useAgents();
  const [servers, setServers] = useState<InstalledMcpView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showDiscover, setShowDiscover] = useState(false);
  const [showAddRemote, setShowAddRemote] = useState(false);
  const [targetAgent, setTargetAgent] = useState<string>(agents[0]?.name ?? "");

  useEffect(() => {
    if (!targetAgent && agents[0]?.name) {
      setTargetAgent(agents[0].name);
    }
  }, [agents, targetAgent]);

  const refresh = useCallback(() => {
    setLoading(true);
    invoke<InstalledMcpView[]>("mcp_installed")
      .then((res) => {
        setServers(res);
        setError(null);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const handleSaved = useCallback(
    (_d: AgentDetail) => {
      refresh();
    },
    [refresh],
  );

  return (
    <div>
      <div
        className="tab-form"
        style={{ display: "flex", gap: 8, alignItems: "flex-start", marginBottom: 12 }}
      >
        <AgentPicker agents={agents} value={targetAgent} onChange={setTargetAgent} />
        <button
          className="btn btn--sm btn--secondary"
          onClick={() => setShowDiscover(true)}
          disabled={!targetAgent}
        >
          {t("detail.discoverMcp")}
        </button>
        <button
          className="btn btn--sm btn--secondary"
          onClick={() => setShowAddRemote(true)}
          disabled={!targetAgent}
        >
          {t("detail.addRemoteMcp")}
        </button>
      </div>

      {error && <p className="save-error">{error}</p>}

      {loading ? (
        <p className="field-muted">{t("mcplib.loading")}</p>
      ) : servers.length === 0 ? (
        <div className="tab-empty">
          <p>{t("mcplib.empty")}</p>
        </div>
      ) : (
        <ul className="item-list">
          {servers.map((s) => (
            <li key={s.id} className="item-card">
              <div className="item-card-name" style={{ display: "flex", gap: 6, alignItems: "center" }}>
                <span style={{ fontWeight: 600 }}>{s.name}</span>
                <span className="item-card__meta">{s.transport}</span>
                <span className="item-card__meta">
                  {t("mcplib.usedBy")} {s.agents.join(", ")}
                </span>
              </div>
              <p style={{ margin: "4px 0 0", fontSize: 13, color: "var(--text-muted, #888)" }}>
                {s.description}
              </p>
            </li>
          ))}
        </ul>
      )}

      {showDiscover && targetAgent && (
        <McpDiscoverModal
          agentName={targetAgent}
          onClose={() => setShowDiscover(false)}
          onImported={(d) => {
            handleSaved(d);
            setShowDiscover(false);
          }}
        />
      )}
      {showAddRemote && targetAgent && (
        <McpAddRemoteModal
          agentName={targetAgent}
          onClose={() => setShowAddRemote(false)}
          onSaved={(d) => {
            handleSaved(d);
            setShowAddRemote(false);
          }}
        />
      )}
    </div>
  );
}
