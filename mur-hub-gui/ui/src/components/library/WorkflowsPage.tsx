import { useT } from "../../i18n";
import { LibraryGlyph } from "./LibraryGlyph";
import { LibraryPage } from "../detail/library/LibraryPage";
import { itemFor, workflowRows, type WorkflowView } from "../detail/library/libraryModel";

// Note: no discover/install section here — workflows arrive in
// `~/.mur/workflows/` automatically (relay-installed or authored locally).
// A server-side shared-registry discovery view is a later concern.

/** Workflows library (spec §3.1): list + detail, Open folder, no agent usage. */
export function WorkflowsPage() {
  const { t } = useT();
  const metaLabels = { path: t("library.meta.path") };
  return (
    <LibraryPage<WorkflowView>
      page="workflows"
      title={t("nav.workflows")}
      listCommand="workflows_list"
      idOf={(w) => w.path}
      rows={(workflows) => workflowRows(workflows, () => <LibraryGlyph kind="workflow" />)}
      item={(w) => itemFor("workflow", w, metaLabels)}
      folderOf={(w) => w.path}
      copy={{
        loading: t("workflowslib.loading"),
        empty: t("workflowslib.empty"),
        filter: t("workflowslib.filter"),
        noMatch: t("workflowslib.noMatch"),
      }}
    />
  );
}
