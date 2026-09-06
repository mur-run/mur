import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../../i18n";
import { SourceList } from "../../shell/SourceList";
import type { SourceFacet, SourceRowData } from "../../shell/sourceListModel";
import type { MenuItemDef } from "../../shell/SplitButton";
import { ListDivider } from "../../shell/ListDivider";
import {
  LIST_WIDTH_DEFAULT, LIST_WIDTH_MAX, LIST_WIDTH_MIN, useResizableColumn,
} from "../../shell/useResizableColumn";
import { listModeFor } from "../../shell/breakpoints";
import { useWindowWidth } from "../../shell/useWindowWidth";
import { readKey, writeKey } from "../../shell/persist";
import { LibraryDetail } from "./LibraryDetail";
import type { LibraryAgentUse, LibraryItem } from "./libraryModel";

export type LibraryPageId = "skills" | "mcp" | "plugins" | "workflows";

export interface LibraryPageProps<T> {
  page: LibraryPageId;
  title: string;
  /** The Tauri command that lists the records. */
  listCommand: string;
  idOf: (r: T) => string;
  rows: (records: T[]) => SourceRowData[];
  facets?: (records: T[]) => SourceFacet[];
  item: (r: T) => LibraryItem;
  /** Undefined for kinds without agent usage. */
  uses?: (r: T) => LibraryAgentUse[];
  /** Per-agent commands; each resolves after the backend applied it. */
  toggle?: (r: T, agent: string, enabled: boolean) => Promise<void>;
  remove?: (r: T, agent: string) => Promise<void>;
  /** Header action: reveal this path in Finder. */
  folderOf?: (r: T) => string | null;
  createLabel?: string;
  createItems?: MenuItemDef[];
  toolbar?: ReactNode;
  copy: { loading: string; empty: string; filter: string; noMatch: string };
  /** Bump to reload after a modal installed something. */
  reloadToken?: number;
  /** Modals, rendered outside the grid. */
  children?: ReactNode;
}

export const libraryKeys = (page: LibraryPageId) => ({
  lastSelected: `mur.${page}.lastSelected`,
  listWidth: `mur.${page}.listWidth`,
});

/** The master–detail Library page (spec §3.1). Owns loading, selection
 *  persistence, filter / facet state and the per-agent action lifecycle; the
 *  four pages only supply builders, commands and modals. */
export function LibraryPage<T>(p: LibraryPageProps<T>) {
  const { t } = useT();
  const keys = libraryKeys(p.page);
  const [records, setRecords] = useState<T[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [facet, setFacet] = useState<string | null>(null);
  const [listShown, setListShown] = useState(false);
  const restored = useRef(false);
  const column = useResizableColumn(keys.listWidth, LIST_WIDTH_DEFAULT, LIST_WIDTH_MIN, LIST_WIDTH_MAX);
  const listMode = listModeFor(useWindowWidth());

  const refresh = useCallback(() => {
    setLoading(true);
    invoke<T[]>(p.listCommand)
      .then((res) => {
        setRecords(res);
        setError(null);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, [p.listCommand]);

  useEffect(() => {
    refresh();
  }, [refresh, p.reloadToken]);

  // One-shot restore once the list exists; a selection that vanished after a
  // refresh clears itself. The write waits for the restore (Phase 1 lesson).
  const idOf = p.idOf;
  useEffect(() => {
    if (records.length === 0) return;
    const ids = records.map(idOf);
    if (!restored.current) {
      restored.current = true;
      const last = readKey(keys.lastSelected);
      if (last && ids.includes(last)) setSelected(last);
      return;
    }
    if (selected && !ids.includes(selected)) setSelected(null);
  }, [records, selected, keys.lastSelected, idOf]);
  useEffect(() => {
    if (restored.current) writeKey(keys.lastSelected, selected);
  }, [selected, keys.lastSelected]);

  async function act(fn: () => Promise<void>) {
    setBusy(true);
    setActionError(null);
    try {
      await fn();
      refresh();
    } catch (e) {
      setActionError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const current = records.find((r) => idOf(r) === selected) ?? null;
  const folder = current && p.folderOf ? p.folderOf(current) : null;
  const toggle = p.toggle;
  const remove = p.remove;
  const cls = `master-detail master-detail--${listMode}${listShown ? " master-detail--list-shown" : ""}`;

  return (
    <div className={cls} style={{ ["--md-list-width" as string]: `${column.width}px` }}>
      <SourceList
        title={p.title}
        count={records.length}
        rows={p.rows(records)}
        facets={p.facets ? p.facets(records) : []}
        allLabel={t("dashboard.all")}
        activeFacet={facet}
        onFacet={setFacet}
        filter={filter}
        onFilter={setFilter}
        filterPlaceholder={p.copy.filter}
        selectedId={selected}
        onSelect={(id) => {
          setSelected(id);
          setListShown(false);
        }}
        createLabel={p.createLabel ?? ""}
        createItems={p.createItems}
        toolbar={p.toolbar}
        emptyState={
          <div className="source-list__empty">
            {loading ? (
              p.copy.loading
            ) : error ? (
              <>
                <p className="save-error">{error}</p>
                <button type="button" className="btn btn--secondary" onClick={refresh}>
                  {t("app.refresh")}
                </button>
              </>
            ) : records.length === 0 ? (
              p.copy.empty
            ) : (
              p.copy.noMatch
            )}
          </div>
        }
      />
      <ListDivider column={column} label={t("shell.resizeList")} />
      <div className="master-detail__detail">
        {listMode === "overlay" && (
          <button
            type="button"
            className="btn btn--secondary master-detail__show-list"
            onClick={() => setListShown((v) => !v)}
          >
            {t("shell.showList")}
          </button>
        )}
        {current ? (
          <LibraryDetail
            key={idOf(current)}
            item={p.item(current)}
            uses={p.uses ? p.uses(current) : undefined}
            busy={busy}
            error={actionError}
            onToggle={
              toggle
                ? (agent, enabled) => {
                    void act(() => toggle(current, agent, enabled));
                  }
                : undefined
            }
            onRemove={
              remove
                ? (agent) => {
                    void act(() => remove(current, agent));
                  }
                : undefined
            }
            onOpenFolder={
              folder
                ? () => {
                    invoke("reveal_in_finder", { path: folder }).catch((e) => setActionError(String(e)));
                  }
                : undefined
            }
          />
        ) : (
          <div className="fleet-view__empty">
            <p>{t("library.selectHint")}</p>
          </div>
        )}
      </div>
      {p.children}
    </div>
  );
}
