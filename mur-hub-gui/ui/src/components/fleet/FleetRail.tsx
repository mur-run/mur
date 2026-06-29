import { useT } from "../../i18n";
import type { FleetSummary } from "./types";

interface Props {
  fleets: FleetSummary[];
  selectedName: string | null;
  onSelect: (name: string) => void;
  onNew: () => void;
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
      <ul className="fleet-rail__list">
        {fleets.length === 0 && (
          <li className="fleet-rail__empty">{t("fleet.empty")}</li>
        )}
        {fleets.map((f) => (
          <li key={f.name}>
            <button
              className={`fleet-rail__item${selectedName === f.name ? " is-selected" : ""}`}
              onClick={() => onSelect(f.name)}
            >
              <div className="fleet-rail__row">
                <span className={`fleet-rail__status ${statusClass(f)}`} />
                <span className="fleet-rail__name">{f.display_name}</span>
                {f.active_jobs > 0 && (
                  <span className="fleet-rail__jobs-badge">{f.active_jobs}</span>
                )}
              </div>
              <div className="fleet-rail__meta">
                {f.member_count} {f.member_count === 1 ? "member" : "members"}
                {f.stopped ? " · stopped" : f.running ? " · running" : " · idle"}
              </div>
            </button>
          </li>
        ))}
      </ul>
    </aside>
  );
}
