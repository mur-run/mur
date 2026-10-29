import type { AgentDetail } from "../../../types";
import { useT } from "../../../i18n";
import { SkillsTab } from "../../inspector/tabs/SkillsTab";
import { McpTab } from "../../inspector/tabs/McpTab";
import { PluginsTab } from "../../inspector/tabs/PluginsTab";
import { PermissionsTab } from "../../inspector/tabs/PermissionsTab";
import { DangerZone } from "./DangerZone";

/** Capabilities = skills + MCP + plugins + permissions (spec §4.3). */
export function CapabilitiesTab({
  detail,
  onSaved,
  isRunning,
  dashboardOnly,
}: {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
  isRunning: boolean;
  /** Only the dashboard may offer a startup override (never a detail window). */
  dashboardOnly?: boolean;
}) {
  const { t } = useT();
  return (
    <>
      <section className="detail-section" id="agent-skills">
        <h3 className="detail-section__title">{t("detail.skills")}</h3>
        <SkillsTab detail={detail} onSaved={onSaved} />
      </section>
      <section className="detail-section" id="agent-mcp">
        <h3 className="detail-section__title">{t("detail.mcp")}</h3>
        <McpTab detail={detail} onSaved={onSaved} />
      </section>
      <section className="detail-section" id="agent-plugins">
        <h3 className="detail-section__title">{t("detail.plugins")}</h3>
        <PluginsTab detail={detail} onSaved={onSaved} />
      </section>
      <section className="detail-section" id="agent-permissions">
        <h3 className="detail-section__title">{t("detail.permissions")}</h3>
        <PermissionsTab detail={detail} onSaved={onSaved} isRunning={isRunning} />
        <DangerZone agentName={detail.agent_name} isRunning={isRunning} dashboardOnly={dashboardOnly} />
      </section>
    </>
  );
}
