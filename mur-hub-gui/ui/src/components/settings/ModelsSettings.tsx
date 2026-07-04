import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import type { TranslationKey } from "../../i18n/types";
import { ModelLibrary } from "../ModelLibrary";
import type { ModelOption } from "../modelPicker";
import type { DetectedLocalView } from "../ModelLibraryPanels";
import { buildSlotGroups, decodeSel, encodeSel, type SlotOptionGroup } from "../modelSlots";

interface SlotView {
  provider: string;
  model: string;
  api_key_ref: string | null;
  health: "ready" | "key_missing" | "unset";
  follows_smart: boolean;
}

interface ModelSlotsView {
  smart: SlotView;
  search: SlotView;
  ask: SlotView;
  compact: SlotView;
  rollup: SlotView;
  summarize: string | null;
  reflector: string | null;
  curator: string | null;
}

export function ModelsSettings() {
  const { t } = useT();
  const [brain, setBrain] = useState<string | null>(null);
  const [slots, setSlots] = useState<ModelSlotsView | null>(null);
  const [groups, setGroups] = useState<SlotOptionGroup[]>([]);
  const [libraryOpen, setLibraryOpen] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const refresh = useCallback(() => {
    invoke<[boolean, string | null]>("nudge_status")
      .then(([, m]) => setBrain(m))
      .catch(() => {});
    invoke<ModelSlotsView>("model_slots_get")
      .then(setSlots)
      .catch(() => {});
    Promise.all([
      invoke<ModelOption[]>("list_models").catch(() => [] as ModelOption[]),
      invoke<DetectedLocalView[]>("probe_local_providers").catch(() => [] as DetectedLocalView[]),
    ]).then(([reg, local]) => setGroups(buildSlotGroups(reg, local)));
  }, []);

  useEffect(refresh, [refresh]);
  // Library writes the same ~/.mur/models.yaml — re-pull once it closes.
  useEffect(() => {
    if (!libraryOpen) refresh();
  }, [libraryOpen, refresh]);

  const setSlot = (slot: string) => (e: React.ChangeEvent<HTMLSelectElement>) => {
    if (!e.target.value) return;
    invoke<ModelSlotsView>("model_slots_set", { slot, sel: decodeSel(e.target.value) })
      .then((v) => {
        setErr(null);
        setSlots(v);
      })
      .catch((x) => setErr(String(x)));
    e.target.value = "";
  };

  const registryGroups = groups.filter((g) => g.options[0]?.payload.kind === "registry");

  const row = (labelKey: TranslationKey, slot: string, view: SlotView, opts?: { localOnly?: boolean }) => (
    <div className="settings-row" key={slot}>
      <span className="settings-row__label">
        {t(labelKey)}
        {opts?.localOnly && <em className="slot-tag">{t("settings.slots.localOnly")}</em>}
      </span>
      <select className="slot-select" value="" onChange={setSlot(slot)} aria-label={t(labelKey)}>
        <option value="">{`${view.provider}/${view.model}`}</option>
        {groups.map((g) => (
          <optgroup key={g.label} label={g.label}>
            {g.options.map((o) => (
              <option key={o.label} value={encodeSel(o.payload)}>
                {o.label}
              </option>
            ))}
          </optgroup>
        ))}
      </select>
      {view.follows_smart ? (
        <span className="slot-health slot-health--ready">{t("settings.slots.follows")}</span>
      ) : (
        <span className={`slot-health slot-health--${view.health}`}>
          {t(`settings.slots.${view.health === "key_missing" ? "keyMissing" : view.health}`)}
        </span>
      )}
    </div>
  );

  const roleRow = (labelKey: TranslationKey, slot: string, refName: string | null) => (
    <div className="settings-row" key={slot}>
      <span className="settings-row__label">{t(labelKey)}</span>
      <select className="slot-select" value="" onChange={setSlot(slot)} aria-label={t(labelKey)}>
        <option value="">{refName ?? t("settings.slots.unset")}</option>
        {registryGroups.map((g) => (
          <optgroup key={g.label} label={g.label}>
            {g.options.map((o) => (
              <option key={o.label} value={encodeSel(o.payload)}>
                {o.label}
              </option>
            ))}
          </optgroup>
        ))}
      </select>
    </div>
  );

  return (
    <section className="settings-section">
      <h3 className="settings-section__title">{t("settings.nav.models")}</h3>
      {slots && (
        <>
          {row("settings.slots.smart", "smart", slots.smart)}
          <p className="settings-hint">{t("settings.slots.smartHint")}</p>
          {row("settings.slots.search", "search", slots.search)}

          <div className="settings-row">
            <span className="settings-row__label">{t("settings.slots.brain")}</span>
            <span className="settings-row__value">{brain ? `🧠 ${brain}` : t("settings.noBrain")}</span>
          </div>

          <details className="slot-advanced">
            <summary>{t("settings.slots.advanced")}</summary>
            {row("conv.ask", "ask", slots.ask)}
            {row("conv.compact", "compact", slots.compact)}
            {row("conv.rollup", "rollup", slots.rollup, { localOnly: true })}
            {roleRow("settings.slots.reflector", "reflector", slots.reflector)}
            {roleRow("settings.slots.curator", "curator", slots.curator)}
          </details>
          {err && <p className="settings-hint slot-error">{err}</p>}
        </>
      )}

      <div className="settings-row">
        <button className="toolbar-btn" onClick={() => setLibraryOpen(true)}>
          {t("settings.openLibrary")}
        </button>
      </div>
      <p className="settings-hint">{t("settings.modelsHint")}</p>
      <ModelLibrary open={libraryOpen} onClose={() => setLibraryOpen(false)} />
    </section>
  );
}
