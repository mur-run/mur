import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { AgentDetail, SkillInstallResult } from "../../../types";
import { useT } from "../../../i18n";
import { SkillAddUrlModal } from "../../SkillAddUrlModal";
import { SkillRegistryModal } from "../../SkillRegistryModal";

export function SkillsTab({
  detail,
  onSaved,
}: {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
}) {
  const { t } = useT();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [installedId, setInstalledId] = useState<string | null>(null);
  const [showSkillUrl, setShowSkillUrl] = useState(false);
  const [showSkillRegistry, setShowSkillRegistry] = useState(false);

  const hasInstalled = detail.installed_skills.length > 0;
  const hasLegacy = detail.skills.length > 0;

  function revealSkill(name: string) {
    invoke("reveal_in_finder", {
      path: `skills/${name}`,
      agent: detail.agent_name,
    }).catch((e) => setError(String(e)));
  }

  async function installSkill() {
    setError(null);
    setInstalledId(null);
    const src = await open({
      multiple: false,
      filters: [{ name: "MUR Skill", extensions: ["yaml", "yml", "md"] }],
    }).catch((e) => {
      setError(String(e));
      return null;
    });
    if (typeof src !== "string" || !src) return;
    setBusy(true);
    try {
      // Backend returns the refreshed detail AND the id the skill was
      // registered as — or rejects with a validation error. Surface the
      // real outcome instead of a blanket "installed" message.
      const res = await invoke<SkillInstallResult>("agent_skill_install", {
        name: detail.agent_name,
        sourcePath: src,
      });
      onSaved(res.detail);
      setInstalledId(res.installed_id);
      setTimeout(() => setInstalledId(null), 6000);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function removeSkill(id: string) {
    setError(null);
    setBusy(true);
    try {
      const updated = await invoke<AgentDetail>("agent_skill_uninstall", {
        name: detail.agent_name,
        skillId: id,
      });
      onSaved(updated);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function toggleSkill(id: string, enabled: boolean) {
    setError(null);
    setBusy(true);
    try {
      const updated = await invoke<AgentDetail>("agent_skill_toggle", {
        name: detail.agent_name,
        skillId: id,
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
      <div style={{ display: "flex", gap: 8, alignSelf: "flex-start" }}>
        <button
          className="btn btn--sm btn--primary"
          onClick={installSkill}
          disabled={busy}
        >
          {t("detail.installSkill")}
        </button>
        <button
          className="btn btn--sm btn--secondary"
          onClick={() => setShowSkillUrl(true)}
          disabled={busy}
        >
          {t("detail.installSkillUrl")}
        </button>
        <button
          className="btn btn--sm btn--secondary"
          onClick={() => setShowSkillRegistry(true)}
          disabled={busy}
        >
          {t("detail.browseRegistry")}
        </button>
      </div>
      <p className="field-muted" style={{ fontSize: 12 }}>
        {t("detail.skillInstallFormatHint")}
      </p>
      {installedId && (
        <p className="save-ok" style={{ fontSize: 12 }}>
          {t("detail.skillInstalledOk", { id: installedId })}
        </p>
      )}
      {error && <p className="save-error">{error}</p>}
      {showSkillUrl && (
        <SkillAddUrlModal
          agentName={detail.agent_name}
          onClose={() => setShowSkillUrl(false)}
          onSaved={onSaved}
        />
      )}
      {showSkillRegistry && (
        <SkillRegistryModal
          agentName={detail.agent_name}
          onClose={() => setShowSkillRegistry(false)}
          onSaved={onSaved}
        />
      )}

      {!hasInstalled && !hasLegacy && (
        <div className="tab-empty">
          <p>{t("detail.noSkills")}</p>
          <p className="field-muted" style={{ fontSize: 12 }}>
            {t("detail.skillInstallHint")}
          </p>
        </div>
      )}

      {hasInstalled && (
        <>
          <label className="field-label" style={{ marginTop: 12 }}>
            {t("detail.installedSkills", { count: detail.installed_skills.length })}
          </label>
          <ul className="item-list">
            {detail.installed_skills.map((s) => (
              <li key={s.name} className={s.enabled ? "item-card" : "item-card item-card-off"}>
                <button
                  className="item-card-remove"
                  title={t("detail.remove")}
                  aria-label={t("detail.remove")}
                  disabled={busy}
                  onClick={() => removeSkill(s.name)}
                >
                  ×
                </button>
                <label className="item-card-toggle" title={s.enabled ? "Disable" : "Enable"}>
                  <input
                    type="checkbox"
                    checked={s.enabled}
                    disabled={busy}
                    onChange={(e) => toggleSkill(s.name, e.target.checked)}
                  />
                </label>
                <div className="item-card-name">
                  {s.name}
                  <button
                    className="btn btn--sm btn--secondary"
                    title={t("detail.reveal")}
                    aria-label={t("detail.reveal")}
                    onClick={() => revealSkill(s.name)}
                    style={{ marginLeft: 6 }}
                  >
                    📁
                  </button>
                </div>
                {s.addon_id && detail.addons.find(a => a.id === s.addon_id)?.enabled === false && (
                  <span className="badge-sm">{t("detail.pluginOff")}</span>
                )}
                {s.version && (
                  <span className="badge-sm">{s.version}</span>
                )}
                {s.description && (
                  <div className="item-card-desc">{s.description}</div>
                )}
                {s.category && (
                  <span className="field-muted" style={{ fontSize: 11 }}>{s.category}</span>
                )}
              </li>
            ))}
          </ul>
        </>
      )}

      {hasLegacy && (
        <>
          <label className="field-label" style={{ marginTop: hasInstalled ? 16 : 12 }}>
            {t("detail.legacySkillPaths", { count: detail.skills.length })}
          </label>
          <ul className="item-list">
            {detail.skills.map((s) => (
              <li key={s.path} className="item-card">
                <button
                  className="item-card-remove"
                  title={t("detail.remove")}
                  aria-label={t("detail.remove")}
                  disabled={busy}
                  onClick={() => removeSkill(s.path)}
                >
                  ×
                </button>
                <div>
                  <code style={{ fontSize: 11 }}>{s.path}</code>
                  <span className={s.loadable ? "badge-loadable" : "badge-dead"}>
                    {s.loadable
                      ? t("detail.skillLoadable")
                      : s.status === "missing"
                        ? t("detail.skillMissing")
                        : t("detail.skillDead")}
                  </span>
                </div>
                {!s.loadable && (
                  <div className="item-card-desc field-muted" style={{ fontSize: 11 }}>
                    {s.status === "missing"
                      ? t("detail.skillMissingHint", { path: s.path })
                      : t("detail.skillDeadHint")}
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
