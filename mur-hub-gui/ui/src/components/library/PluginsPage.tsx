import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useAgents } from "../../context/AgentContext";
import { useT } from "../../i18n";
import { AgentPicker } from "./AgentPicker";
import { LibraryGlyph } from "./LibraryGlyph";
import { LibraryPage } from "../detail/library/LibraryPage";
import { itemFor, pluginRows, type InstalledAddonAgg } from "../detail/library/libraryModel";
import { useInstallTarget } from "../detail/library/useInstallTarget";

/** Plugins (add-ons) library (spec §3.1). `addons_installed` carries the real
 *  per-agent enabled flag, so the toggle reflects it. */
export function PluginsPage() {
  const { t } = useT();
  const { agents } = useAgents();
  const [target, setTarget] = useInstallTarget(agents);
  const [reload, setReload] = useState(0);
  const [importError, setImportError] = useState<string | null>(null);
  const metaLabels = {
    source: t("library.meta.source"),
    skills: t("library.meta.skills"),
    mcp: t("library.meta.mcp"),
    commands: t("library.meta.commands"),
  };

  async function importPlugin() {
    if (!target) return;
    setImportError(null);
    try {
      const dir = await open({ directory: true, title: t("pluginslib.import") });
      if (!dir || Array.isArray(dir)) return;
      await invoke("agent_addon_import", { name: target, pluginDir: dir, force: false });
      setReload((n) => n + 1);
    } catch (e) {
      setImportError(String(e));
    }
  }

  return (
    <LibraryPage<InstalledAddonAgg>
      page="plugins"
      title={t("nav.plugins")}
      listCommand="addons_installed"
      idOf={(a) => a.id}
      rows={(addons) =>
        pluginRows(
          addons,
          { skills: t("pluginslib.skills"), mcp: t("pluginslib.mcp"), commands: t("pluginslib.commands") },
          () => <LibraryGlyph kind="plugin" />,
        )
      }
      item={(a) => itemFor("plugin", a, metaLabels)}
      uses={(a) => a.agents.map(({ agent, enabled }) => ({ agent, enabled }))}
      toggle={async (a, agent, enabled) => {
        await invoke("agent_addon_toggle", { name: agent, addonId: a.id, enabled });
      }}
      remove={async (a, agent) => {
        await invoke("agent_addon_remove", { name: agent, addonId: a.id });
      }}
      folderOf={(a) => a.source}
      createLabel={t("pluginslib.import")}
      createItems={[
        { id: "import", label: t("pluginslib.import"), onSelect: () => { void importPlugin(); }, disabled: !target },
      ]}
      toolbar={
        <div className="library-picker">
          <AgentPicker agents={agents} value={target} onChange={setTarget} />
          {importError && <p className="save-error">{importError}</p>}
        </div>
      }
      copy={{
        loading: t("pluginslib.loading"),
        empty: t("pluginslib.empty"),
        filter: t("pluginslib.filter"),
        noMatch: t("pluginslib.noMatch"),
      }}
      reloadToken={reload}
    />
  );
}
