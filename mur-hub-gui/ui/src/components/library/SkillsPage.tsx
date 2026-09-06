import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAgents } from "../../context/AgentContext";
import { useT } from "../../i18n";
import { SkillRegistryModal } from "../SkillRegistryModal";
import { SkillAddUrlModal } from "../SkillAddUrlModal";
import { AgentPicker } from "./AgentPicker";
import { LibraryGlyph } from "./LibraryGlyph";
import { LibraryPage } from "../detail/library/LibraryPage";
import { itemFor, skillFacets, skillRows, type InstalledSkillView } from "../detail/library/libraryModel";
import { useInstallTarget } from "../detail/library/useInstallTarget";

/** Skills library (spec §3.1): installed skills | detail with usage.
 *  `skills_installed` does not report per-agent enabled state, so the toggle
 *  starts checked and only the unchecked direction reaches the backend;
 *  re-enabling is done from the agent's Capabilities tab. */
export function SkillsPage() {
  const { t } = useT();
  const { agents } = useAgents();
  const [target, setTarget] = useInstallTarget(agents);
  const [showRegistry, setShowRegistry] = useState(false);
  const [showAddUrl, setShowAddUrl] = useState(false);
  const [reload, setReload] = useState(0);
  const metaLabels = {
    category: t("library.meta.category"),
    version: t("library.meta.version"),
    status: t("library.meta.status"),
    path: t("library.meta.path"),
  };

  return (
    <LibraryPage<InstalledSkillView>
      page="skills"
      title={t("nav.skills")}
      listCommand="skills_installed"
      idOf={(s) => s.name}
      rows={(skills) => skillRows(skills, t("library.versionPrefix"), () => <LibraryGlyph kind="skill" />)}
      facets={skillFacets}
      item={(s) => itemFor("skill", s, metaLabels)}
      uses={(s) => s.agents.map((agent) => ({ agent, enabled: true }))}
      toggle={async (s, agent, enabled) => {
        await invoke("agent_skill_toggle", { name: agent, skillId: s.name, enabled });
      }}
      remove={async (s, agent) => {
        await invoke("agent_skill_uninstall", { name: agent, skillId: s.name });
      }}
      folderOf={(s) => s.path}
      createLabel={t("skillslib.add")}
      createItems={[
        { id: "url", label: t("detail.installSkillUrl"), onSelect: () => setShowAddUrl(true), disabled: !target },
        { id: "registry", label: t("detail.browseRegistry"), onSelect: () => setShowRegistry(true), disabled: !target },
      ]}
      toolbar={
        <div className="library-picker">
          <AgentPicker agents={agents} value={target} onChange={setTarget} />
        </div>
      }
      copy={{
        loading: t("skillslib.loading"),
        empty: t("detail.noSkills"),
        filter: t("skillslib.filter"),
        noMatch: t("skillslib.noMatch"),
      }}
      reloadToken={reload}
    >
      {showAddUrl && target && (
        <SkillAddUrlModal
          agentName={target}
          onClose={() => setShowAddUrl(false)}
          onSaved={() => {
            setReload((n) => n + 1);
            setShowAddUrl(false);
          }}
        />
      )}
      {showRegistry && target && (
        <SkillRegistryModal
          agentName={target}
          onClose={() => setShowRegistry(false)}
          onSaved={() => {
            setReload((n) => n + 1);
            setShowRegistry(false);
          }}
        />
      )}
    </LibraryPage>
  );
}
