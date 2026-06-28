import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../i18n";
import type { AgentDetail } from "../types";

interface SkillPreview {
  name: string;
  description: string;
  category: string;
  body: string;
  blocking: boolean;
  findings: string[];
}

interface Props {
  agentName: string;
  onClose: () => void;
  onSaved: (d: AgentDetail) => void;
}

export function SkillAddUrlModal({ agentName, onClose, onSaved }: Props) {
  const { t } = useT();
  const [url, setUrl] = useState("");
  const [previews, setPreviews] = useState<SkillPreview[] | null>(null);
  const [accept, setAccept] = useState(false);
  const [busy, setBusy] = useState<null | "fetch" | "install">(null);
  const [error, setError] = useState<string | null>(null);

  async function fetchPreview() {
    setError(null);
    setPreviews(null);
    setAccept(false);
    const trimmed = url.trim();
    if (
      !(
        trimmed.startsWith("https://") ||
        trimmed.startsWith("http://localhost") ||
        trimmed.startsWith("http://127.0.0.1") ||
        trimmed.startsWith("http://[::1]")
      )
    ) {
      setError(t("skillurl.invalidUrl"));
      return;
    }
    setBusy("fetch");
    try {
      setPreviews(await invoke<SkillPreview[]>("agent_skill_preview_url", { url: trimmed }));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  }

  async function install() {
    setError(null);
    setBusy("install");
    try {
      const res = await invoke<{ detail: AgentDetail; installed_ids: string[] }>(
        "agent_skill_install_url",
        { name: agentName, url: url.trim(), acceptFindings: accept },
      );
      onSaved(res.detail);
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  }

  const anyBlocking = !!previews && previews.some((p) => p.blocking);
  const canInstall = !!previews && previews.length > 0 && busy === null && (!anyBlocking || accept);

  return (
    <div className="modal__overlay" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal__header">
          <h2 className="modal__title">{t("skillurl.title")}</h2>
          <button className="modal__close" onClick={onClose} aria-label={t("detail.close")}>×</button>
        </div>
        <div className="modal__body">
          <label className="field-label">{t("skillurl.url")}</label>
          <div style={{ display: "flex", gap: 8 }}>
            <input
              className="input"
              type="url"
              value={url}
              onChange={(e) => { setUrl(e.target.value); setPreviews(null); setAccept(false); }}
              onKeyDown={(e) => e.key === "Enter" && !busy && fetchPreview()}
              placeholder={t("skillurl.urlPlaceholder")}
              style={{ flex: 1 }}
              autoFocus
            />
            <button
              className="btn btn--sm btn--secondary"
              onClick={fetchPreview}
              disabled={busy !== null}
            >
              {busy === "fetch" ? t("skillurl.fetching") : t("skillurl.fetch")}
            </button>
          </div>

          {previews && (
            <div style={{ marginTop: 12 }}>
              <p className="field-label">{t("skillurl.previewHeading")}</p>
              {previews.length > 1 && (
                <p className="field-muted" style={{ marginTop: 0 }}>
                  {t("skillurl.bundleHint")}
                </p>
              )}
              {previews.map((p, i) => (
                <div key={i} className="item-card" style={{ marginTop: 8 }}>
                  <span className="item-card__name">{p.name}</span>
                  <span className="item-card__meta">{p.category}</span>
                  <p style={{ margin: "4px 0 0", fontSize: 13 }}>
                    {p.description}
                  </p>
                  {p.findings.length > 0 && (
                    <div style={{ marginTop: 8 }}>
                      <p className="field-label" style={{ marginTop: 0 }}>{t("skillurl.findingsHeading")}</p>
                      <ul className="item-list">
                        {p.findings.map((f, j) => (
                          <li key={j} className="save-error">
                            {f}
                          </li>
                        ))}
                      </ul>
                    </div>
                  )}
                  <div style={{ marginTop: 8 }}>
                    <p className="field-muted" style={{ marginTop: 0 }}>
                      {t("skillurl.bodyHeading")}
                    </p>
                    <pre
                      className="item-card"
                      style={{ whiteSpace: "pre-wrap", maxHeight: 160, overflow: "auto", fontSize: 12 }}
                    >
                      {p.body}
                    </pre>
                  </div>
                </div>
              ))}
              {anyBlocking && (
                <label
                  style={{ display: "flex", gap: 8, alignItems: "center", marginTop: 8, fontSize: 13 }}
                >
                  <input
                    type="checkbox"
                    checked={accept}
                    onChange={(e) => setAccept(e.target.checked)}
                  />
                  {t("skillurl.accept")}
                </label>
              )}
            </div>
          )}

          {error && <p className="save-error">{error}</p>}
          {previews && (
            <p className="field-muted" style={{ marginTop: 8 }}>
              {t("skillurl.restartHint")}
            </p>
          )}
        </div>
        <div className="modal__footer">
          <button
            className="btn btn--sm btn--secondary"
            onClick={onClose}
          >
            {t("detail.cancel")}
          </button>
          <button
            className="btn btn--sm btn--primary"
            disabled={!canInstall}
            onClick={install}
          >
            {busy === "install" ? t("skillurl.installing") : t("skillurl.install")}
          </button>
        </div>
      </div>
    </div>
  );
}
