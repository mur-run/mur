import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAgents } from "../../context/AgentContext";
import type { AgentDetail } from "../../types";
import { useT } from "../../i18n";
import { SkillRegistryModal } from "../SkillRegistryModal";
import { SkillAddUrlModal } from "../SkillAddUrlModal";

// ─── Backend shape ───────────────────────────────────────────────────────────

interface InstalledSkillView {
  name: string;
  description: string;
  category: string;
  origin_version: string | null;
  status: string;
}

// ─── Status → badge variant ──────────────────────────────────────────────────

/** Pure mapping: backend status label -> badge CSS variant. Exported for
 * testing; the backend already reduces `UpgradeStatus` to these labels. */
export function statusBadgeClass(status: string): string {
  switch (status) {
    case "modified":
      return "badge badge--warn";
    case "update available":
      return "badge badge--warn";
    case "up to date":
      return "badge badge--ok";
    default:
      return "badge";
  }
}

// ─── Component ────────────────────────────────────────────────────────────────

export function SkillsPage() {
  const { t } = useT();
  const { agents } = useAgents();
  const [skills, setSkills] = useState<InstalledSkillView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showRegistry, setShowRegistry] = useState(false);
  const [showAddUrl, setShowAddUrl] = useState(false);

  const refresh = useCallback(() => {
    setLoading(true);
    invoke<InstalledSkillView[]>("skills_installed")
      .then((res) => {
        setSkills(res);
        setError(null);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // The registry/URL-install modals are per-agent (they install into an
  // agent's own skill dir); reused as-is here targeting the first available
  // agent, since this library view has no separate agent-scoped concept.
  const targetAgent = agents[0]?.name;

  function handleSaved(_detail: AgentDetail) {
    refresh();
  }

  return (
    <div className="tab-form">
      <div style={{ display: "flex", gap: 8, alignSelf: "flex-start", marginBottom: 12 }}>
        <button
          className="btn btn--sm btn--secondary"
          onClick={() => setShowAddUrl(true)}
          disabled={!targetAgent}
        >
          {t("detail.installSkillUrl")}
        </button>
        <button
          className="btn btn--sm btn--secondary"
          onClick={() => setShowRegistry(true)}
          disabled={!targetAgent}
        >
          {t("detail.browseRegistry")}
        </button>
      </div>

      {error && <p className="save-error">{error}</p>}

      {loading ? (
        <p className="field-muted">{t("skillslib.loading")}</p>
      ) : skills.length === 0 ? (
        <div className="tab-empty">
          <p>{t("detail.noSkills")}</p>
        </div>
      ) : (
        <ul className="item-list">
          {skills.map((s) => (
            <li key={s.name} className="item-card">
              <div className="item-card-name" style={{ display: "flex", gap: 6, alignItems: "center" }}>
                <span style={{ fontWeight: 600 }}>{s.name}</span>
                <span className="item-card__meta">{s.category}</span>
                {s.origin_version && <span className="item-card__meta">v{s.origin_version}</span>}
                <span className={statusBadgeClass(s.status)}>{s.status}</span>
              </div>
              <p style={{ margin: "4px 0 0", fontSize: 13, color: "var(--text-muted, #888)" }}>
                {s.description}
              </p>
            </li>
          ))}
        </ul>
      )}

      {showAddUrl && targetAgent && (
        <SkillAddUrlModal
          agentName={targetAgent}
          onClose={() => setShowAddUrl(false)}
          onSaved={(d) => {
            handleSaved(d);
            setShowAddUrl(false);
          }}
        />
      )}
      {showRegistry && targetAgent && (
        <SkillRegistryModal
          agentName={targetAgent}
          onClose={() => setShowRegistry(false)}
          onSaved={(d) => {
            handleSaved(d);
            setShowRegistry(false);
          }}
        />
      )}
    </div>
  );
}
