//! Library inspector — contextual right-pane for the Library pages
//! (Skills / MCP / Workflows / Plugins). Renders the selected item's manifest
//! detail: name, version, origin, and description/readme. The data is the item
//! the page already has in hand — no extra fetch.

import { useT } from "../../i18n";

export interface LibrarySelection {
  kind: "skill" | "mcp" | "workflow" | "plugin";
  name: string;
  version?: string | null;
  origin?: string | null;
  description?: string | null;
  /** Extra key/value rows (e.g. category, transport, agent count). */
  meta?: { label: string; value: string }[];
}

const KIND_LABEL: Record<LibrarySelection["kind"], string> = {
  skill: "libraryInspector.kind.skill",
  mcp: "libraryInspector.kind.mcp",
  workflow: "libraryInspector.kind.workflow",
  plugin: "libraryInspector.kind.plugin",
};

interface Props {
  item: LibrarySelection;
  onClose: () => void;
}

export function LibraryInspector({ item, onClose }: Props) {
  const { t } = useT();
  return (
    <aside className="detail-panel detail-panel--inspector">
      <div className="detail-panel__header">
        <div className="detail-panel__top">
          <div className="detail-panel__ident">
            <div className="detail-panel__name">{item.name}</div>
            <span className="field-muted" style={{ fontSize: 12 }}>{t(KIND_LABEL[item.kind])}</span>
          </div>
          <button
            className="detail-panel__close"
            onClick={onClose}
            title={t("detail.close")}
            aria-label={t("detail.close")}
          >
            ×
          </button>
        </div>
      </div>
      <div className="detail-panel__body">
        <div className="tab-form">
          {item.version && (
            <>
              <label className="field-label">{t("libraryInspector.version")}</label>
              <p className="field-value">{item.version}</p>
            </>
          )}
          {item.origin && (
            <>
              <label className="field-label" style={{ marginTop: 12 }}>{t("libraryInspector.origin")}</label>
              <code className="item-card-code">{item.origin}</code>
            </>
          )}
          {item.meta?.map((m) => (
            <div key={m.label}>
              <label className="field-label" style={{ marginTop: 12 }}>{m.label}</label>
              <p className="field-value">{m.value}</p>
            </div>
          ))}
          {item.description && (
            <>
              <label className="field-label" style={{ marginTop: 12 }}>{t("libraryInspector.readme")}</label>
              <p className="field-value" style={{ whiteSpace: "pre-wrap" }}>{item.description}</p>
            </>
          )}
        </div>
      </div>
    </aside>
  );
}
