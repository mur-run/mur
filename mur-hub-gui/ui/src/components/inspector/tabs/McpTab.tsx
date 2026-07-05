import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { AgentDetail } from "../../../types";
import { useT } from "../../../i18n";
import { McpDiscoverModal } from "../../McpDiscoverModal";
import { McpAddRemoteModal } from "../../McpAddRemoteModal";

export function McpTab({
  detail,
  onSaved,
}: {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
}) {
  const { t } = useT();
  const [showForm, setShowForm] = useState(false);
  const [serverId, setServerId] = useState("");
  const [command, setCommand] = useState("");
  const [args, setArgs] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [justAdded, setJustAdded] = useState(false);
  const [showDiscover, setShowDiscover] = useState(false);
  const [showAddRemote, setShowAddRemote] = useState(false);

  function reveal(path: string, agent?: string) {
    invoke("reveal_in_finder", { path, agent }).catch((e) => setError(String(e)));
  }

  async function browseCommand() {
    const picked = await open({ multiple: false }).catch(() => null);
    if (typeof picked === "string" && picked) setCommand(picked);
  }

  async function addServer() {
    setError(null);
    setBusy(true);
    try {
      const updated = await invoke<AgentDetail>("agent_mcp_add", {
        name: detail.agent_name,
        serverId: serverId.trim(),
        command: command.trim(),
        args: args.trim() ? args.trim().split(/\s+/) : [],
      });
      onSaved(updated);
      setShowForm(false);
      setServerId("");
      setCommand("");
      setArgs("");
      setJustAdded(true);
      setTimeout(() => setJustAdded(false), 4000);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function removeServer(id: string) {
    setError(null);
    setBusy(true);
    try {
      const updated = await invoke<AgentDetail>("agent_mcp_remove", {
        name: detail.agent_name,
        serverId: id,
      });
      onSaved(updated);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function toggleServer(id: string, enabled: boolean) {
    setError(null);
    setBusy(true);
    try {
      const updated = await invoke<AgentDetail>("agent_mcp_toggle", {
        name: detail.agent_name,
        serverId: id,
        enabled,
      });
      onSaved(updated);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="tab-form">
      {!showForm && (
        <div style={{ display: "flex", gap: 8, alignSelf: "flex-start" }}>
          <button className="btn btn--sm btn--primary" onClick={() => setShowForm(true)}>
            {t("detail.addMcp")}
          </button>
          <button className="btn btn--sm btn--secondary" onClick={() => setShowDiscover(true)}>
            {t("detail.discoverMcp")}
          </button>
          <button className="btn btn--sm btn--secondary" onClick={() => setShowAddRemote(true)}>
            {t("detail.addRemoteMcp")}
          </button>
        </div>
      )}
      {showDiscover && (
        <McpDiscoverModal
          agentName={detail.agent_name}
          onClose={() => setShowDiscover(false)}
          onImported={onSaved}
        />
      )}
      {showAddRemote && (
        <McpAddRemoteModal
          agentName={detail.agent_name}
          onClose={() => setShowAddRemote(false)}
          onSaved={onSaved}
        />
      )}
      {justAdded && (
        <p className="field-muted" style={{ fontSize: 12 }}>
          {t("detail.mcpAddedHint")}
        </p>
      )}

      {showForm && (
        <div className="mcp-add-form">
          <label className="field-label">{t("detail.mcpId")}</label>
          <input
            className="input"
            value={serverId}
            placeholder="media"
            onChange={(e) => setServerId(e.target.value)}
          />
          <label className="field-label">{t("detail.mcpCommand")}</label>
          <div className="mcp-command-row">
            <input
              className="input"
              value={command}
              placeholder="/usr/local/bin/my-mcp-server"
              onChange={(e) => setCommand(e.target.value)}
            />
            <button className="toolbar-btn" onClick={browseCommand} disabled={busy}>
              {t("detail.browse")}
            </button>
          </div>
          <label className="field-label">{t("detail.mcpArgs")}</label>
          <input
            className="input"
            value={args}
            placeholder="--flag value"
            onChange={(e) => setArgs(e.target.value)}
          />
          <div className="mcp-form-actions">
            <button
              className="btn btn--sm btn--primary"
              onClick={addServer}
              disabled={busy || !serverId.trim() || !command.trim()}
            >
              {busy ? t("detail.saving") : t("detail.add")}
            </button>
            <button
              className="btn btn--sm btn--secondary"
              onClick={() => {
                setShowForm(false);
                setError(null);
              }}
              disabled={busy}
            >
              {t("detail.cancel")}
            </button>
          </div>
        </div>
      )}
      {error && <p className="save-error">{error}</p>}

      {detail.mcp_servers.length === 0 ? (
        <div className="tab-empty">
          <p>{t("detail.noMcp")}</p>
          <p className="field-muted" style={{ fontSize: 12 }}>
            {t("detail.mcpAddHint")}
          </p>
        </div>
      ) : (
        <>
          <label className="field-label" style={{ marginTop: 12 }}>
            {t("detail.mcpServersCount", { count: detail.mcp_servers.length })}
          </label>
          <ul className="item-list">
            {detail.mcp_servers.map((m) => (
              <li key={m.name} className={m.enabled ? "item-card" : "item-card item-card-off"}>
                <button
                  className="item-card-remove"
                  title={t("detail.remove")}
                  aria-label={t("detail.remove")}
                  disabled={busy}
                  onClick={() => removeServer(m.name)}
                >
                  ×
                </button>
                <label className="item-card-toggle" title={m.enabled ? "Disable" : "Enable"}>
                  <input
                    type="checkbox"
                    checked={m.enabled}
                    disabled={busy}
                    onChange={(e) => toggleServer(m.name, e.target.checked)}
                  />
                </label>
                <div className="item-card-name">{m.name}</div>
                {m.addon_id && detail.addons.find(a => a.id === m.addon_id)?.enabled === false && (
                  <span className="badge-sm">{t("detail.pluginOff")}</span>
                )}
                <code className="item-card-code">{m.command}</code>
                {m.command.includes("/") && (
                  <button
                    className="btn btn--sm btn--secondary"
                    title={t("detail.reveal")}
                    aria-label={t("detail.reveal")}
                    onClick={() => reveal(m.command)}
                    style={{ marginLeft: 6 }}
                  >
                    📁
                  </button>
                )}
                {m.args.length > 0 && (
                  <div className="item-card-args">
                    {m.args.map((a, i) => (
                      <span key={i} className="badge-sm">{a}</span>
                    ))}
                  </div>
                )}
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}
