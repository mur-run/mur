import { useEffect, useRef, type KeyboardEvent, type ReactNode } from "react";
import { NeedsYouBadge, StatusDot } from "./Status";
import { filterRows, moveSelection, type SourceFacet, type SourceRowData } from "./sourceListModel";

/** ⌘F focuses this list's filter field. One SourceList is mounted per page. */
export function isFilterShortcut(e: globalThis.KeyboardEvent): boolean {
  return (e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "f";
}

export interface SourceListProps {
  title: string;
  count: number;
  rows: SourceRowData[];
  facets: SourceFacet[];
  allLabel: string;
  activeFacet: string | null;
  onFacet: (id: string | null) => void;
  filter: string;
  onFilter: (q: string) => void;
  filterPlaceholder: string;
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  onCreate: () => void;
  createLabel: string;
  emptyState: ReactNode;
}

/** The list pane every master–detail page shares (spec §4.1): header with a
 *  "+" action, a ⌘F filter, facet chips, and status-aware rows. */
export function SourceList(p: SourceListProps) {
  const visible = filterRows(p.rows, p.filter, p.activeFacet);
  const filterRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    function onKey(e: globalThis.KeyboardEvent) {
      if (isFilterShortcut(e)) {
        e.preventDefault();
        filterRef.current?.focus();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  function onListKey(e: KeyboardEvent<HTMLDivElement>) {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      p.onSelect(moveSelection(visible, p.selectedId, e.key === "ArrowDown" ? 1 : -1));
    } else if (e.key === "Escape") {
      p.onSelect(null);
    }
  }

  return (
    <section className="source-list" aria-label={p.title}>
      <header className="source-list__head">
        <h2 className="source-list__title">
          {p.title} <span className="source-list__count">{p.count}</span>
        </h2>
        <button type="button" className="source-list__create" onClick={p.onCreate} title={p.createLabel} aria-label={p.createLabel}>
          +
        </button>
      </header>
      <input
        ref={filterRef}
        className="source-list__filter"
        type="search"
        value={p.filter}
        placeholder={p.filterPlaceholder}
        onChange={(e) => p.onFilter(e.target.value)}
      />
      {p.facets.length > 0 && (
        <div className="source-list__chips" role="group">
          <button type="button" className={`chip${p.activeFacet === null ? " chip--on" : ""}`} onClick={() => p.onFacet(null)}>
            {p.allLabel} <i>{p.count}</i>
          </button>
          {p.facets.map((f) => (
            <button
              key={f.id}
              type="button"
              className={`chip${p.activeFacet === f.id ? " chip--on" : ""}`}
              onClick={() => p.onFacet(p.activeFacet === f.id ? null : f.id)}
            >
              {f.label} <i>{f.count}</i>
            </button>
          ))}
        </div>
      )}
      <div
        className="source-list__rows"
        role="listbox"
        tabIndex={0}
        aria-activedescendant={p.selectedId ? `row-${p.selectedId}` : undefined}
        onKeyDown={onListKey}
      >
        {visible.length === 0
          ? p.emptyState
          : visible.map((r) => (
              <div
                key={r.id}
                id={`row-${r.id}`}
                role="option"
                aria-selected={r.id === p.selectedId}
                className={`source-row${r.id === p.selectedId ? " source-row--on" : ""}`}
                onClick={() => p.onSelect(r.id)}
              >
                <span className="source-row__avatar">{r.avatar}</span>
                <span className="source-row__text">
                  <span className="source-row__name">{r.name}</span>
                  {r.subtitle && <span className="source-row__sub">{r.subtitle}</span>}
                </span>
                <span className="source-row__status">
                  <NeedsYouBadge count={r.needsYou ?? 0} />
                  <StatusDot kind={r.status} />
                </span>
              </div>
            ))}
      </div>
    </section>
  );
}
