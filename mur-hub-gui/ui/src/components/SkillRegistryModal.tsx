import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../i18n";
import type { AgentDetail } from "../types";

// ─── Backend shapes ───────────────────────────────────────────────────────────

interface RegistrySkillEntryView {
  name: string;
  description: string;
  publisher: string;
  category: string;
  latest: string;
  signed_in_index: boolean;
}

interface SigView {
  status: string; // "verified" | "unsigned" | "invalid"
  publisher: string;
  key_fp: string;
}

interface ConsentInfo {
  name: string;
  version: string;
  publisher: string;
  category: string;
  signature: SigView;
  hash: string;
  mcp_requirements: string[];
  findings: string[];
  blocking: boolean;
  needs_ack: boolean;
  /** Content-scan has blocking findings — requires --yes acknowledgement. */
  scan_blocking: boolean;
  trust_level: string;
  body: string;
}

// ─── Props ────────────────────────────────────────────────────────────────────

interface Props {
  agentName: string;
  onClose: () => void;
  onSaved: (d: AgentDetail) => void;
}

// ─── Component ───────────────────────────────────────────────────────────────

export function SkillRegistryModal({ agentName, onClose, onSaved }: Props) {
  const { t } = useT();
  const [query, setQuery]     = useState("");
  const [results, setResults] = useState<RegistrySkillEntryView[] | null>(null);
  const [consent, setConsent] = useState<ConsentInfo | null>(null);
  const [accept, setAccept]   = useState(false);
  const [busy, setBusy]       = useState<null | "search" | "preview" | "install">(null);
  const [error, setError]     = useState<string | null>(null);

  async function search() {
    setError(null);
    setResults(null);
    setConsent(null);
    setAccept(false);
    setBusy("search");
    try {
      setResults(await invoke<RegistrySkillEntryView[]>("agent_skill_registry_search", { query }));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  }

  async function preview(skillName: string) {
    setError(null);
    setConsent(null);
    setAccept(false);
    setBusy("preview");
    try {
      setConsent(await invoke<ConsentInfo>("agent_skill_registry_preview", { skill: skillName }));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  }

  async function install() {
    if (!consent) return;
    setError(null);
    setBusy("install");
    try {
      const detail = await invoke<AgentDetail>("agent_skill_registry_install", {
        name: agentName,
        skill: consent.name,
        version: consent.version,
        accept,
      });
      onSaved(detail);
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  }

  // Install is enabled when: not verify-blocking, AND (no ack needed OR ack given).
  // Ack is needed for: unsigned/absent-hash (needs_ack) OR scan findings (scan_blocking).
  const canInstall = !!consent && !consent.blocking && (!(consent.needs_ack || consent.scan_blocking) || accept);

  const sigLabel = consent
    ? (consent.signature.status === "verified" ? t("skillreg.sigVerified")
      : consent.signature.status === "invalid"  ? t("skillreg.sigInvalid")
      : t("skillreg.sigUnsigned"))
    : "";
  const sigCls = consent
    ? (consent.signature.status === "verified" ? "badge badge--ok"
      : consent.signature.status === "invalid"  ? "badge badge--err"
      : "badge badge--warn")
    : "";

  return (
    <div className="modal__overlay" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal__header">
          <h2 className="modal__title">{t("skillreg.title")}</h2>
          <button className="modal__close" onClick={onClose} aria-label={t("detail.close")}>×</button>
        </div>

        <div className="modal__body">
          {/* ── Search bar ── */}
          <div style={{ display: "flex", gap: 8 }}>
            <input
              className="input"
              type="search"
              value={query}
              onChange={(e) => { setQuery(e.target.value); setResults(null); setConsent(null); }}
              onKeyDown={(e) => e.key === "Enter" && !busy && search()}
              placeholder={t("skillreg.searchPlaceholder")}
              style={{ flex: 1 }}
              autoFocus
            />
            <button
              className="btn btn--sm btn--secondary"
              onClick={search}
              disabled={busy !== null}
            >
              {busy === "search" ? t("skillreg.searching") : t("skillreg.search")}
            </button>
          </div>

          {/* ── Results list ── */}
          {results !== null && !consent && (
            <div style={{ marginTop: 12 }}>
              {results.length === 0 ? (
                <p className="field-muted">{t("skillreg.noResults")}</p>
              ) : (
                <>
                  <p className="field-label">{t("skillreg.resultsHeading")}</p>
                  <ul className="item-list">
                    {results.map((r) => (
                      <li
                        key={r.name}
                        className="item-card"
                        style={{ cursor: busy === "preview" ? "wait" : "pointer" }}
                        onClick={() => busy === null && preview(r.name)}
                        title={t("skillreg.clickToReview")}
                      >
                        <div className="item-card-name" style={{ display: "flex", gap: 6, alignItems: "center" }}>
                          <span style={{ fontWeight: 600 }}>{r.name}</span>
                          <span className="item-card__meta">{r.category}</span>
                          <span className="item-card__meta">{r.latest}</span>
                          {r.signed_in_index && (
                            <span className="badge badge--ok">{t("skillreg.signedBadge")}</span>
                          )}
                        </div>
                        <p style={{ margin: "4px 0 0", fontSize: 13, color: "var(--text-muted, #888)" }}>
                          {r.description}
                        </p>
                        <p style={{ margin: "2px 0 0", fontSize: 12, color: "var(--text-muted, #888)" }}>
                          {r.publisher}
                        </p>
                      </li>
                    ))}
                  </ul>
                </>
              )}
            </div>
          )}

          {/* ── Consent view ── */}
          {consent && (
            <div style={{ marginTop: 12 }}>
              <button
                className="btn btn--sm btn--secondary"
                style={{ marginBottom: 10 }}
                onClick={() => { setConsent(null); setAccept(false); }}
                disabled={busy !== null}
              >
                ← {t("skillreg.backToResults")}
              </button>

              <div className="item-card" style={{ marginBottom: 8 }}>
                <div style={{ display: "flex", gap: 8, alignItems: "center", flexWrap: "wrap" }}>
                  <span style={{ fontWeight: 600, fontSize: 14 }}>{consent.name}</span>
                  <span className="item-card__meta">{consent.category}</span>
                  <span className="item-card__meta">v{consent.version}</span>
                  <span className={sigCls}>{sigLabel}</span>
                  {consent.signature.status === "verified" && consent.signature.key_fp && (
                    <span className="field-muted" style={{ fontSize: 11, fontFamily: "monospace" }}>
                      {consent.signature.key_fp}
                    </span>
                  )}
                </div>
                <p style={{ margin: "4px 0 0", fontSize: 13 }}>
                  <span className="field-label" style={{ marginRight: 4 }}>{t("skillreg.publisher")}:</span>
                  {consent.publisher}
                </p>
                <p style={{ margin: "2px 0 0", fontSize: 12 }}>
                  <span className="field-label" style={{ marginRight: 4 }}>{t("skillreg.hash")}:</span>
                  <code style={{ fontSize: 11 }}>{consent.hash || t("skillreg.hashAbsent")}</code>
                </p>
                <p style={{ margin: "2px 0 0", fontSize: 12 }}>
                  <span className="field-label" style={{ marginRight: 4 }}>{t("skillreg.trustLevel")}:</span>
                  {consent.trust_level}
                </p>
              </div>

              {consent.mcp_requirements.length > 0 && (
                <div style={{ marginTop: 8 }}>
                  <p className="field-label" style={{ marginTop: 0 }}>{t("skillreg.mcpRequirements")}</p>
                  <ul className="item-list">
                    {consent.mcp_requirements.map((req, i) => (
                      <li key={i} className="item-card" style={{ fontSize: 13 }}>{req}</li>
                    ))}
                  </ul>
                </div>
              )}

              {consent.findings.length > 0 && (
                <div style={{ marginTop: 8 }}>
                  <p className="field-label" style={{ marginTop: 0 }}>{t("skillreg.findingsHeading")}</p>
                  <ul className="item-list">
                    {consent.findings.map((f, i) => (
                      <li key={i} className="save-error">{f}</li>
                    ))}
                  </ul>
                </div>
              )}

              {consent.blocking && (
                <p className="save-error" style={{ marginTop: 8 }}>
                  {t("skillreg.blockedRefusal")}
                </p>
              )}

              <div style={{ marginTop: 8 }}>
                <p className="field-label" style={{ marginTop: 0 }}>{t("skillurl.bodyHeading")}</p>
                <pre
                  className="item-card"
                  style={{ whiteSpace: "pre-wrap", maxHeight: 180, overflow: "auto", fontSize: 12 }}
                >
                  {consent.body}
                </pre>
              </div>

              {(consent.needs_ack || consent.scan_blocking) && !consent.blocking && (
                <label
                  style={{ display: "flex", gap: 8, alignItems: "center", marginTop: 8, fontSize: 13 }}
                >
                  <input
                    type="checkbox"
                    checked={accept}
                    onChange={(e) => setAccept(e.target.checked)}
                  />
                  {t("skillreg.installAnyway")}
                </label>
              )}

              {error && <p className="save-error" style={{ marginTop: 8 }}>{error}</p>}
              <p className="field-muted" style={{ marginTop: 8 }}>
                {t("skillurl.restartHint")}
              </p>
            </div>
          )}

          {error && !consent && <p className="save-error" style={{ marginTop: 8 }}>{error}</p>}
        </div>

        <div className="modal__footer">
          <button className="btn btn--sm btn--secondary" onClick={onClose}>
            {t("detail.cancel")}
          </button>
          {consent && (
            <button
              className="btn btn--sm btn--primary"
              disabled={!canInstall || busy !== null}
              onClick={install}
            >
              {busy === "install" ? t("skillurl.installing") : t("skillreg.install")}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
