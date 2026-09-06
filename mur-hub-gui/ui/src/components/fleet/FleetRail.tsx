import { useT } from "../../i18n";
import type { FleetSummary, LabelView } from "./types";
import { filterByLabels, groupFleets, UNGROUPED } from "./fleetLabels";
import { StatusDot, fleetStatusOf } from "../shell/Status";

interface Props {
  fleets: FleetSummary[];
  labels: LabelView[];
  selectedLabels: string[];
  onToggleLabel: (id: string) => void;
  onClearLabels: () => void;
  selectedName: string | null;
  onSelect: (name: string) => void;
  onNew: () => void;
}

export function FleetRail({
  fleets,
  labels,
  selectedLabels,
  onToggleLabel,
  onClearLabels,
  selectedName,
  onSelect,
  onNew,
}: Props) {
  const { t } = useT();
  const visible = filterByLabels(fleets, selectedLabels);
  const groups = groupFleets(visible, labels);

  return (
    <aside className="fleet-rail">
      <button className="fleet-rail__new toolbar-btn toolbar-btn--primary" onClick={onNew}>
        + {t("fleet.new")}
      </button>
      {labels.length > 0 && (
        <div className="fleet-rail__chips">
          <button
            className={`fleet-chip${selectedLabels.length === 0 ? " is-active" : ""}`}
            onClick={onClearLabels}
          >
            {t("fleet.labelAll")}
          </button>
          {labels.map((l) => (
            <button
              key={l.id}
              className={`fleet-chip${selectedLabels.includes(l.id) ? " is-active" : ""}`}
              style={l.color ? { borderColor: l.color } : undefined}
              onClick={() => onToggleLabel(l.id)}
              title={l.display || l.id}
            >
              {l.display || l.id}
              <span className="fleet-chip__count">{l.fleet_count}</span>
            </button>
          ))}
        </div>
      )}
      <ul className="fleet-rail__list">
        {visible.length === 0 && (
          <li className="fleet-rail__empty">
            {fleets.length === 0 ? t("fleet.empty") : t("fleet.labelNoMatch")}
          </li>
        )}
        {groups.map((g) => (
          <li key={g.id} className="fleet-rail__group">
            <div className="fleet-rail__group-title">
              {g.color && (
                <span className="fleet-rail__group-dot" style={{ background: g.color }} />
              )}
              {g.id === UNGROUPED ? t("fleet.labelUngrouped") : g.title}
              <span className="fleet-rail__group-count">{g.fleets.length}</span>
            </div>
            <ul className="fleet-rail__group-list">
              {g.fleets.map((f) => (
                <li key={f.name}>
                  <button
                    className={`fleet-rail__item${selectedName === f.name ? " is-selected" : ""}`}
                    onClick={() => onSelect(f.name)}
                  >
                    <div className="fleet-rail__row">
                      <StatusDot kind={fleetStatusOf(f)} />
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
          </li>
        ))}
      </ul>
    </aside>
  );
}
