import type { AgentDetail } from "../../../types";
import { useT } from "../../../i18n";

export function PermissionsTab({ detail }: { detail: AgentDetail }) {
  const { t } = useT();
  return (
    <div className="tab-form">
      <label className="field-label">{t("detail.capabilities")}</label>
      {detail.capabilities.length === 0 ? (
        <p className="field-muted" style={{ fontSize: 12 }}>{t("detail.noCaps")}</p>
      ) : (
        <div className="badge-row">
          {detail.capabilities.map((c) => (
            <span key={c} className="cap-tag"><span className="cap-dot" />{c}</span>
          ))}
        </div>
      )}

      <label className="field-label" style={{ marginTop: 16 }}>{t("detail.mcpServers")}</label>
      <p className="field-muted" style={{ fontSize: 12 }}>
        {detail.mcp_servers.length === 0
          ? t("detail.noMcp")
          : t("detail.mcpSummary", { count: detail.mcp_servers.length })}
      </p>

      <label className="field-label" style={{ marginTop: 16 }}>{t("detail.skills")}</label>
      <p className="field-muted" style={{ fontSize: 12 }}>
        {detail.installed_skills.length === 0 && detail.skills.length === 0
          ? t("detail.noSkills")
          : t("detail.skillsSummary", {
              installed: detail.installed_skills.length,
              legacy: detail.skills.length,
            })}
      </p>

      <p className="field-muted" style={{ marginTop: 24, fontSize: 11, fontStyle: "italic" }}>
        {t("detail.permissionsHint")}
      </p>
    </div>
  );
}
