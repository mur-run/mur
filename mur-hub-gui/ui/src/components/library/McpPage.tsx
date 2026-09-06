import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAgents } from "../../context/AgentContext";
import { useT } from "../../i18n";
import { McpDiscoverModal } from "../McpDiscoverModal";
import { McpAddRemoteModal } from "../McpAddRemoteModal";
import { AgentPicker } from "./AgentPicker";
import { LibraryGlyph } from "./LibraryGlyph";
import { LibraryPage } from "../detail/library/LibraryPage";
import { itemFor, mcpFacets, mcpRows, type InstalledMcpView } from "../detail/library/libraryModel";
import { useInstallTarget } from "../detail/library/useInstallTarget";

/** MCP library (spec §3.1). `mcp_installed` reports which agents configure a
 *  server but not the per-agent enabled flag, so the toggle starts checked. */
export function McpPage() {
  const { t } = useT();
  const { agents } = useAgents();
  const [target, setTarget] = useInstallTarget(agents);
  const [showDiscover, setShowDiscover] = useState(false);
  const [showAddRemote, setShowAddRemote] = useState(false);
  const [reload, setReload] = useState(0);
  const metaLabels = { transport: t("library.meta.transport"), serverId: t("library.meta.serverId") };

  return (
    <LibraryPage<InstalledMcpView>
      page="mcp"
      title={t("nav.mcp")}
      listCommand="mcp_installed"
      idOf={(s) => s.id}
      rows={(servers) => mcpRows(servers, (n) => t("library.usedByCount", { count: n }), () => <LibraryGlyph kind="mcp" />)}
      facets={mcpFacets}
      item={(s) => itemFor("mcp", s, metaLabels)}
      uses={(s) => s.agents.map((agent) => ({ agent, enabled: true }))}
      toggle={async (s, agent, enabled) => {
        await invoke("agent_mcp_toggle", { name: agent, serverId: s.id, enabled });
      }}
      remove={async (s, agent) => {
        await invoke("agent_mcp_remove", { name: agent, serverId: s.id });
      }}
      createLabel={t("mcplib.add")}
      createItems={[
        { id: "discover", label: t("detail.discoverMcp"), onSelect: () => setShowDiscover(true), disabled: !target },
        { id: "remote", label: t("detail.addRemoteMcp"), onSelect: () => setShowAddRemote(true), disabled: !target },
      ]}
      toolbar={
        <div className="library-picker">
          <AgentPicker agents={agents} value={target} onChange={setTarget} />
        </div>
      }
      copy={{
        loading: t("mcplib.loading"),
        empty: t("mcplib.empty"),
        filter: t("mcplib.filter"),
        noMatch: t("mcplib.noMatch"),
      }}
      reloadToken={reload}
    >
      {showDiscover && target && (
        <McpDiscoverModal
          agentName={target}
          onClose={() => setShowDiscover(false)}
          onImported={() => {
            setReload((n) => n + 1);
            setShowDiscover(false);
          }}
        />
      )}
      {showAddRemote && target && (
        <McpAddRemoteModal
          agentName={target}
          onClose={() => setShowAddRemote(false)}
          onSaved={() => {
            setReload((n) => n + 1);
            setShowAddRemote(false);
          }}
        />
      )}
    </LibraryPage>
  );
}
