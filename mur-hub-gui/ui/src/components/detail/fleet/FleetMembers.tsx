import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AgentEntry } from "../../../types";
import type { FleetDetail as Detail, LabelView } from "../../fleet/types";
import { useT } from "../../../i18n";
import { CATEGORY_COLORS, avatarPreset, familyOf } from "../../../utils";
import { PetFace } from "../../PetFace";
import { labelIdFrom, makePrimary, toggleAssignment } from "../../fleet/fleetLabels";
import { showToast, useFleetCall } from "./fleetActions";

export interface FleetMembersProps {
  detail: Detail;
  agentMap: Map<string, AgentEntry>;
  /** The whole registry, in registry order — the chips offered here. */
  labels: LabelView[];
  /** This fleet's assigned label ids, primary first. */
  fleetLabels: string[];
  onRefresh: () => void;
}

/** Members tab (spec §4.4): the labels and members blocks of the old FleetDetail. */
export function FleetMembers({ detail, agentMap, labels, fleetLabels, onRefresh }: FleetMembersProps) {
  const { t } = useT();
  const { busy, setBusy, call } = useFleetCall(onRefresh);
  const [addInput, setAddInput] = useState("");
  const [newLabel, setNewLabel] = useState("");

  async function saveLabels(ids: string[]) {
    setBusy("fleet_set_labels");
    try {
      await invoke("fleet_set_labels", { name: detail.name, ids });
      onRefresh(); // reloads the list so the chips regroup immediately
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  // Create a label and assign it to this fleet in one go: a label made from
  // the fleet you are looking at is one you want on that fleet. It lands last,
  // so it only becomes the group (primary) if this fleet had none.
  async function createLabel() {
    const display = newLabel.trim();
    if (display === "") return;
    const id = labelIdFrom(display, labels.map((l) => l.id));
    setBusy("fleet_label_create");
    try {
      await invoke("fleet_label_create", { id, display, color: null });
      await invoke("fleet_set_labels", {
        name: detail.name,
        ids: toggleAssignment(fleetLabels, id),
      });
      setNewLabel("");
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  async function handleAddMember() {
    const agent = addInput.trim();
    if (!agent) return;
    setBusy("fleet_add_member");
    try {
      await invoke("fleet_add_member", { name: detail.name, agent });
      setAddInput("");
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  return (
    <>
      <section className="detail-section" id="fleet-labels">
        <h3 className="detail-section__title">{t("fleet.labels")}</h3>
        {labels.length > 0 && (
          <>
            <div className="fleet-labels">
              {labels.map((l) => {
                const on = fleetLabels.includes(l.id);
                const primary = fleetLabels[0] === l.id;
                return (
                  <button
                    key={l.id}
                    className={`fleet-chip${on ? " is-active" : ""}${primary ? " is-primary" : ""}`}
                    style={l.color ? { borderColor: l.color } : undefined}
                    disabled={busy !== null}
                    title={primary ? t("fleet.labelPrimary") : t("fleet.labelMakePrimary")}
                    onClick={(e) => {
                      // Plain click toggles; alt/⌥-click promotes to primary.
                      const next = e.altKey
                        ? makePrimary(fleetLabels, l.id)
                        : toggleAssignment(fleetLabels, l.id);
                      void saveLabels(next);
                    }}
                  >
                    {primary && <span className="fleet-chip__pin">★</span>}
                    {l.display || l.id}
                  </button>
                );
              })}
            </div>
            <div className="fleet-labels__hint">{t("fleet.labelHint")}</div>
          </>
        )}
        <div className="fleet-labels__new">
          <input
            value={newLabel}
            onChange={(e) => setNewLabel(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void createLabel();
              if (e.key === "Escape") setNewLabel("");
            }}
            placeholder={t("fleet.labelNew")}
            autoComplete="off"
            disabled={busy !== null}
          />
          <button
            className="toolbar-btn"
            onClick={() => void createLabel()}
            disabled={busy !== null || newLabel.trim() === ""}
          >
            +
          </button>
        </div>
        {labels.length === 0 && (
          <div className="fleet-labels__empty">{t("fleet.labelsEmpty")}</div>
        )}
      </section>

      <section className="detail-section" id="fleet-members">
        <h3 className="detail-section__title">{t("fleet.members")}</h3>
        <div className="fleet-members">
          {detail.members.map((m) => {
            const agent = agentMap.get(m) ?? agentMap.get(m.toLowerCase());
            const color = agent ? (CATEGORY_COLORS[agent.category] ?? "#6B7280") : "#6B7280";
            return (
              <div key={m} className="fleet-member">
                <div className="fleet-member__avatar" style={agent ? {} : { background: color }}>
                  {agent ? (
                    <PetFace presetId={avatarPreset(agent)} family={familyOf(avatarPreset(agent))} expression="idle" size={24} animate={false} />
                  ) : (
                    <span style={{ fontSize: 12, color: "#fff", fontWeight: 600 }}>
                      {m.charAt(0).toUpperCase()}
                    </span>
                  )}
                </div>
                <span className="fleet-member__name">{agent?.display_name ?? m}</span>
                <button
                  className="fleet-member__remove"
                  onClick={() => call("fleet_remove_member", { name: detail.name, agent: m })}
                  disabled={busy !== null}
                >
                  ✕
                </button>
              </div>
            );
          })}
        </div>
        {/* Add member: searchable combobox */}
        <div className="fleet-add-member">
          <div className="fleet-add-member__combo">
            <input
              value={addInput}
              onChange={(e) => { setAddInput(e.target.value); }}
              placeholder={t("fleet.addMember")}
              onKeyDown={(e) => {
                if (e.key === "Enter") handleAddMember();
                if (e.key === "Escape") setAddInput("");
              }}
              autoComplete="off"
            />
            {addInput.length > 0 && (() => {
              const lower = addInput.toLowerCase();
              const memberSet = new Set(detail.members.map((m) => m.toLowerCase()));
              const suggestions = Array.from(agentMap.values()).filter(
                (a) => !memberSet.has(a.name.toLowerCase()) &&
                  (a.name.toLowerCase().includes(lower) || a.display_name.toLowerCase().includes(lower)),
              );
              return suggestions.length > 0 ? (
                <ul className="fleet-add-member__suggestions">
                  {suggestions.map((a) => (
                    <li key={a.name} onMouseDown={() => { setAddInput(a.name); }}>
                      {a.display_name}
                    </li>
                  ))}
                </ul>
              ) : null;
            })()}
          </div>
          <button className="toolbar-btn" onClick={handleAddMember} disabled={busy !== null || !addInput.trim()}>+</button>
        </div>
      </section>
    </>
  );
}
