//! Add remote (Streamable-HTTP) MCP server by URL: validate → test
//! connection → review the server's full tool descriptions → add.
//! Bearer tokens go to keychain (backend); OAuth opens the system browser.

import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../i18n";
import type { AgentDetail } from "../types";

type Auth = "none" | "bearer" | "oauth";

interface ProbeTool {
  name: string;
  description: string;
  input_schema: unknown;
}

interface ProbeOutcome {
  transport: "streamable_http" | "legacy_sse" | "unknown";
  needs_auth: boolean;
  resource_metadata: string | null;
  tools: ProbeTool[];
}

interface Props {
  agentName: string;
  onClose: () => void;
  onSaved: (d: AgentDetail) => void;
}

export function McpAddRemoteModal({ agentName, onClose, onSaved }: Props) {
  const { t } = useT();
  const [url, setUrl] = useState("");
  const [serverId, setServerId] = useState("");
  const [auth, setAuth] = useState<Auth>("none");
  const [token, setToken] = useState("");
  const [probe, setProbe] = useState<ProbeOutcome | null>(null);
  const [busy, setBusy] = useState<null | "test" | "add">(null);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  async function test() {
    setError(null);
    setBusy("test");
    setProbe(null);
    try {
      const out = await invoke<ProbeOutcome>("agent_mcp_test_connection", {
        url,
        bearer: auth === "bearer" && token ? token : null,
      });
      setProbe(out);
      if (out.needs_auth && auth === "none") setAuth("oauth");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  }

  async function add() {
    setError(null);
    setBusy("add");
    try {
      if (auth === "oauth") {
        const detail = await invoke<AgentDetail>("agent_mcp_oauth_login", {
          name: agentName,
          serverId,
          url,
        });
        setSaved(true);
        onSaved(detail);
      } else {
        const detail = await invoke<AgentDetail>("agent_mcp_add_remote", {
          name: agentName,
          serverId,
          url,
          bearer: auth === "bearer" && token ? token : null,
        });
        setSaved(true);
        onSaved(detail);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  }

  const authLabels: Record<Auth, string> = {
    none: t("remote.authNone"),
    bearer: t("remote.authBearer"),
    oauth: t("remote.authOauth"),
  };

  return (
    <div className="modal__overlay" onClick={onClose}>
      <div className="modal__box" onClick={(e) => e.stopPropagation()}>
        <div className="modal__header">
          <h2 className="modal__title">{t("remote.title")}</h2>
          <button className="modal__close" onClick={onClose} aria-label={t("detail.close")}>
            ×
          </button>
        </div>
        <div className="modal__body">
          <label className="field-muted">{t("remote.url")}</label>
          <input
            className="input"
            type="url"
            placeholder={t("remote.urlPlaceholder")}
            value={url}
            onChange={(e) => {
              setUrl(e.target.value);
              setProbe(null);
              setSaved(false);
            }}
            autoFocus
          />

          <div style={{ marginTop: 8 }}>
            <label className="field-muted">{t("detail.mcpId")}</label>
            <input
              className="input"
              type="text"
              placeholder="my-mcp-server"
              value={serverId}
              onChange={(e) => setServerId(e.target.value)}
            />
          </div>

          <div style={{ marginTop: 10 }}>
            <label className="field-muted">{t("remote.auth")}</label>
            <div className="mcp-form-actions">
              {(["none", "bearer", "oauth"] as Auth[]).map((a) => (
                <label key={a} style={{ marginRight: 12 }}>
                  <input
                    type="radio"
                    name="auth"
                    checked={auth === a}
                    onChange={() => setAuth(a)}
                  />{" "}
                  {authLabels[a]}
                </label>
              ))}
            </div>
          </div>

          {auth === "bearer" && (
            <input
              className="input"
              type="password"
              placeholder={t("remote.token")}
              value={token}
              onChange={(e) => setToken(e.target.value)}
              style={{ marginTop: 8 }}
            />
          )}

          <div className="mcp-form-actions" style={{ marginTop: 12 }}>
            <button
              className="btn btn--sm btn--secondary"
              disabled={!url || !serverId || busy !== null}
              onClick={test}
            >
              {busy === "test" ? t("remote.testing") : t("remote.test")}
            </button>
          </div>

          {probe && (
            <div style={{ marginTop: 12 }}>
              {probe.transport === "legacy_sse" && (
                <p className="field-muted" style={{ color: "var(--color-warn, #c07a00)" }}>
                  {t("remote.legacyWarn")}
                </p>
              )}
              {probe.needs_auth && auth === "none" && (
                <p className="field-muted" style={{ color: "var(--color-warn, #c07a00)" }}>
                  {t("remote.needsAuth")}
                </p>
              )}
              {!probe.needs_auth && (
                <p className="field-muted" style={{ color: "var(--color-ok, #2a7a2a)" }}>
                  {t("remote.testOk")}
                </p>
              )}

              <p className="field-muted" style={{ marginTop: 8, fontWeight: 600 }}>
                {t("remote.toolsHeading")}
              </p>
              {probe.tools.length === 0 ? (
                <p className="field-muted">{t("remote.noTools")}</p>
              ) : (
                <ul className="item-list" style={{ marginTop: 4 }}>
                  {probe.tools.map((tool) => (
                    <li key={tool.name} className="item-card">
                      <span style={{ fontWeight: 600 }}>{tool.name}</span>
                      {tool.description && (
                        <span className="field-muted" style={{ marginLeft: 8 }}>
                          {tool.description}
                        </span>
                      )}
                    </li>
                  ))}
                </ul>
              )}
            </div>
          )}

          {error && (
            <p className="field-muted" style={{ color: "var(--color-error, #c00)", marginTop: 8 }}>
              {error}
            </p>
          )}

          {saved && (
            <p className="field-muted" style={{ color: "var(--color-ok, #2a7a2a)", marginTop: 8 }}>
              {t("remote.restartHint")}
            </p>
          )}
        </div>

        <div className="modal__footer">
          {auth === "oauth" ? (
            <button
              className="btn btn--sm btn--primary"
              disabled={!url || !serverId || busy !== null || saved}
              onClick={add}
            >
              {busy === "add" ? t("remote.adding") : t("remote.oauthLogin")}
            </button>
          ) : (
            <button
              className="btn btn--sm btn--primary"
              disabled={!url || !serverId || !probe || busy !== null || saved}
              onClick={add}
            >
              {busy === "add" ? t("remote.adding") : t("remote.add")}
            </button>
          )}
          <button className="btn btn--sm btn--secondary" onClick={onClose}>
            {t("detail.close")}
          </button>
        </div>
      </div>
    </div>
  );
}
