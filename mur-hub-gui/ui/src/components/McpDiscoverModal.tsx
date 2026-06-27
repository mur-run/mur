//! #2 — Discover MCP servers already configured in *other* tools (Claude
//! Desktop/Code, Cursor, VS Code, Windsurf, Antigravity, Gemini CLI, Codex) and
//! import a chosen one into this agent. Import reuses the existing
//! `agent_mcp_add` command; the 📁 button reveals a server's source config in
//! Finder via `reveal_in_finder` (#3).

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../i18n";
import type { AgentDetail } from "../types";

interface DiscoveredServer {
  client: string;
  name: string;
  command: string;
  args: string[];
  source: string;
}

interface Props {
  agentName: string;
  onClose: () => void;
  onImported: (detail: AgentDetail) => void;
}

export function McpDiscoverModal({ agentName, onClose, onImported }: Props) {
  const { t } = useT();
  const [servers, setServers] = useState<DiscoveredServer[] | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [imported, setImported] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<DiscoveredServer[]>("mcp_discover")
      .then(setServers)
      .catch((e) => {
        setError(String(e));
        setServers([]);
      });
  }, []);

  async function importOne(s: DiscoveredServer, key: string) {
    setError(null);
    setBusy(key);
    try {
      const detail = await invoke<AgentDetail>("agent_mcp_add", {
        name: agentName,
        serverId: s.name,
        command: s.command,
        args: s.args,
      });
      onImported(detail);
      setImported((prev) => new Set(prev).add(key));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  }

  function reveal(path: string) {
    invoke("reveal_in_finder", { path }).catch((e) => setError(String(e)));
  }

  return (
    <div className="modal__overlay" onClick={onClose}>
      <div className="modal modal--wide" onClick={(e) => e.stopPropagation()}>
        <div className="modal__header">
          <h2 className="modal__title">{t("detail.discoverTitle")}</h2>
          <button className="modal__close" onClick={onClose} aria-label={t("detail.close")}>
            ×
          </button>
        </div>
        <div className="modal__body">
          {servers === null && <p className="field-muted">{t("detail.discoverScanning")}</p>}
          {servers !== null && servers.length === 0 && !error && (
            <p className="field-muted">{t("detail.discoverEmpty")}</p>
          )}
          {error && <p className="save-error">{error}</p>}
          {servers && servers.length > 0 && (
            <ul className="item-list">
              {servers.map((s) => {
                const key = `${s.client}/${s.name}`;
                const done = imported.has(key);
                const remote = s.command.length === 0;
                return (
                  <li key={key} className="item-card">
                    <div className="item-card-name">
                      {s.name} <span className="badge-sm">{s.client}</span>
                    </div>
                    <code className="item-card-code">
                      {remote ? "(remote/http)" : [s.command, ...s.args].join(" ")}
                    </code>
                    <div className="mcp-form-actions" style={{ marginTop: 6 }}>
                      <button
                        className="btn btn--sm btn--primary"
                        disabled={done || remote || busy !== null}
                        onClick={() => importOne(s, key)}
                      >
                        {done
                          ? t("detail.imported")
                          : busy === key
                            ? t("detail.saving")
                            : t("detail.import")}
                      </button>
                      <button
                        className="btn btn--sm btn--secondary"
                        title={t("detail.reveal")}
                        aria-label={t("detail.reveal")}
                        onClick={() => reveal(s.source)}
                      >
                        📁
                      </button>
                    </div>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
        <div className="modal__footer" style={{ padding: 12, textAlign: "right" }}>
          <button className="btn btn--sm btn--secondary" onClick={onClose}>
            {t("detail.close")}
          </button>
        </div>
      </div>
    </div>
  );
}
