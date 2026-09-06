import { useT } from "../../../i18n";
import type { TranslationKey } from "../../../i18n/types";
import { DetailPage } from "../../shell/DetailPage";
import { LibraryGlyph } from "../../library/LibraryGlyph";
import type { LibraryAgentUse, LibraryItem } from "./libraryModel";

const KIND_LABEL: Record<LibraryItem["kind"], TranslationKey> = {
  skill: "libraryInspector.kind.skill",
  mcp: "libraryInspector.kind.mcp",
  workflow: "libraryInspector.kind.workflow",
  plugin: "libraryInspector.kind.plugin",
};

export interface LibraryDetailProps {
  item: LibraryItem;
  /** Omit (undefined) for kinds without agent usage (workflows). */
  uses?: LibraryAgentUse[];
  busy: boolean;
  error: string | null;
  onToggle?: (agent: string, enabled: boolean) => void;
  onRemove?: (agent: string) => void;
  onOpenFolder?: () => void;
}

/** The shared Library detail (spec §3.2): description, meta rows, and the
 *  agents that use the item with per-agent enable / remove. */
export function LibraryDetail({ item, uses, busy, error, onToggle, onRemove, onOpenFolder }: LibraryDetailProps) {
  const { t } = useT();
  return (
    <DetailPage
      avatar={<LibraryGlyph kind={item.kind} large />}
      title={item.name}
      meta={<span>{t(KIND_LABEL[item.kind])}</span>}
      actions={
        item.path && onOpenFolder ? (
          <button type="button" className="btn btn--secondary" onClick={onOpenFolder}>
            {t("workflowslib.openFolder")}
          </button>
        ) : undefined
      }
      tabs={[{ id: "overview", label: t("detail.tab.overview") }]}
      activeTab="overview"
      onTab={() => {}}
    >
      {item.description && (
        <div className="detail-card">
          <div className="detail-card__eyebrow">{t("libraryInspector.readme")}</div>
          <p className="library-detail__desc">{item.description}</p>
        </div>
      )}
      <div className="detail-card">
        {item.meta.map((m) => (
          <div key={m.label} className="detail-kv">
            <span>{m.label}</span>
            <span className={m.mono ? "mono" : undefined}>{m.value}</span>
            <span />
          </div>
        ))}
      </div>
      {uses && (
        <div className="detail-card">
          <div className="detail-card__eyebrow">{t("library.usedBy")}</div>
          {uses.length === 0 && <p className="library-detail__muted">{t("library.notUsed")}</p>}
          {uses.map((u) => (
            <div key={u.agent} className="library-use">
              {u.enabled !== undefined && onToggle ? (
                <label className="library-use__toggle">
                  <input
                    type="checkbox"
                    checked={u.enabled}
                    disabled={busy}
                    onChange={(e) => onToggle(u.agent, e.target.checked)}
                  />
                  <span>{u.agent}</span>
                </label>
              ) : (
                <span>{u.agent}</span>
              )}
              {onRemove && (
                <button type="button" className="btn btn--secondary" disabled={busy} onClick={() => onRemove(u.agent)}>
                  {t("pluginslib.remove")}
                </button>
              )}
            </div>
          ))}
          {error && <p className="save-error">{error}</p>}
        </div>
      )}
    </DetailPage>
  );
}
