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
  const [preview, setPreview] = useState<SkillPreview | null>(null);
  const [accept, setAccept] = useState(false);
  const [busy, setBusy] = useState<null | "fetch" | "install">(null);
  const [error, setError] = useState<string | null>(null);

  async function fetchPreview() {
    setError(null);
    setPreview(null);
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
      setPreview(
        await invoke<SkillPreview>("agent_skill_preview_url", { url: trimmed }),
      );
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
      const detail = await invoke<{ detail: AgentDetail; installed_id: string }>(
        "agent_skill_install_url",
        { name: agentName, url: url.trim(), acceptFindings: accept },
      );
      onSaved(detail.detail);
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  }

  const canInstall = !!preview && busy === null && (!preview.blocking || accept);

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
              onChange={(e) => { setUrl(e.target.value); setPreview(null); setAccept(false); }}
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

          {preview && (
            <div style={{ marginTop: 12 }}>
              <p className="field-label">{t("skillurl.previewHeading")}</p>
              <div className="item-card">
                <span className="item-card__name">{preview.name}</span>
                <span className="item-card__meta">{preview.category}</span>
                <p style={{ margin: "4px 0 0", fontSize: 13 }}>
                  {preview.description}
                </p>
              </div>

              {preview.findings.length > 0 && (
                <div style={{ marginTop: 10 }}>
                  <p className="field-label">{t("skillurl.findingsHeading")}</p>
                  <ul className="item-list">
                    {preview.findings.map((f, i) => (
                      <li key={i} className="save-error">
                        {f}
                      </li>
                    ))}
                  </ul>
                  {preview.blocking && (
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

              <div style={{ marginTop: 10 }}>
                <p className="field-muted" style={{ marginTop: 0 }}>
                  {t("skillurl.bodyHeading")}
                </p>
                <pre
                  className="item-card"
                  style={{ whiteSpace: "pre-wrap", maxHeight: 240, overflow: "auto", fontSize: 12 }}
                >
                  {preview.body}
                </pre>
              </div>
            </div>
          )}

          {error && <p className="save-error">{error}</p>}
          {preview && (
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
