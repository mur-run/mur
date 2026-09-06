import { useState } from "react";
import { useT } from "../../i18n";
import { showToast } from "../detail/fleet/fleetActions";
import { StatusDot } from "./Status";
import { bulkCounts, startableIds, stoppableIds, type BulkItem, type BulkResult } from "./bulkModel";

export interface BulkPanelProps {
  /** The selection, in list order. */
  items: BulkItem[];
  /** Page-specific commands over the given ids; resolve with one result per id. */
  onStart: (ids: string[]) => Promise<BulkResult[]>;
  onStop: (ids: string[]) => Promise<BulkResult[]>;
  /** Back to the anchor alone. */
  onClear: () => void;
}

type BulkKind = "start" | "stop";

/** The detail column while two or more rows are selected (spec 3(c) §6). */
export function BulkPanel({ items, onStart, onStop, onClear }: BulkPanelProps) {
  const { t } = useT();
  const [busy, setBusy] = useState<BulkKind | null>(null);
  const [results, setResults] = useState<ReadonlyMap<string, BulkResult>>(new Map());
  const counts = bulkCounts(items);

  async function run(kind: BulkKind) {
    const ids = kind === "start" ? startableIds(items) : stoppableIds(items);
    setBusy(kind);
    setResults(new Map());
    try {
      const out = await (kind === "start" ? onStart(ids) : onStop(ids));
      setResults(new Map(out.map((r) => [r.id, r])));
      const ok = out.filter((r) => r.ok).length;
      showToast(t(kind === "start" ? "bulk.startedSummary" : "bulk.stoppedSummary", { ok, failed: out.length - ok }));
    } finally {
      setBusy(null);
    }
  }

  return (
    <section className="bulk">
      <h2 className="bulk__title">{t("bulk.selected", { count: items.length })}</h2>
      <div className="bulk__actions">
        <button type="button" className="btn btn--primary" disabled={busy !== null || counts.startable === 0} onClick={() => void run("start")}>
          {t("bulk.start", { count: counts.startable })}
        </button>
        <button type="button" className="btn btn--secondary" disabled={busy !== null || counts.stoppable === 0} onClick={() => void run("stop")}>
          {t("bulk.stop", { count: counts.stoppable })}
        </button>
        <button type="button" className="btn btn--link" onClick={onClear}>
          {t("bulk.clear")}
        </button>
      </div>
      <ul className="bulk__list">
        {items.map((it) => {
          const r = results.get(it.id);
          return (
            <li key={it.id} className="bulk__row">
              <StatusDot kind={it.status} />
              <span className="bulk__name">{it.name}</span>
              {r && (
                <span className={`bulk__result${r.ok ? "" : " bulk__result--failed"}`} aria-label={r.ok ? "ok" : r.error}>
                  {r.ok ? "✓" : "✗"}
                </span>
              )}
              {r && !r.ok && r.error && <span className="bulk__error">{r.error}</span>}
            </li>
          );
        })}
      </ul>
    </section>
  );
}
