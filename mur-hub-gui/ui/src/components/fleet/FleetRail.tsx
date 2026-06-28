import { useT } from "../../i18n";
import type { FleetSummary } from "./types";

interface Props {
  fleets: FleetSummary[];
  selectedName: string | null;
  onSelect: (name: string) => void;
  onNew: () => void;
}

function statusBadge(f: FleetSummary): string {
  if (f.stopped) return "⏸";
  if (f.running) return "▶";
  return "●";
}

function statusClass(f: FleetSummary): string {
  if (f.stopped) return "fleet-rail__status--stopped";
  if (f.running) return "fleet-rail__status--running";
  return "fleet-rail__status--idle";
}

export function FleetRail({ fleets, selectedName, onSelect, onNew }: Props) {
  const { t } = useT();
  return (
    <aside className="fleet-rail">
      <button className="fleet-rail__new toolbar-btn toolbar-btn--primary" onClick={onNew}>
        + {t("fleet.new")}
      </button>
      {fleets.length === 0 && (
        <p className="fleet-rail__empty">{t("fleet.empty")}</p>
      )}
      <ul className="fleet-rail__list">
        {fleets.map((f) => (
          <li
            key={f.name}
            className={`fleet-rail__item${selectedName === f.name ? " is-selected" : ""}`}
            onClick={() => onSelect(f.name)}
          >
            <span className={`fleet-rail__status ${statusClass(f)}`}>
              {statusBadge(f)}
            </span>
            <span className="fleet-rail__name">{f.display_name}</span>
            {f.active_jobs > 0 && (
              <span className="fleet-rail__jobs">{f.active_jobs}</span>
            )}
          </li>
        ))}
      </ul>
    </aside>
  );
}
