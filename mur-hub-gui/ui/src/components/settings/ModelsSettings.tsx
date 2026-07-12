import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import type { TranslationKey } from "../../i18n/types";
import { ModelLibrary } from "../ModelLibrary";
import type { ModelOption } from "../modelPicker";
import type { DetectedLocalView } from "../ModelLibraryPanels";
import { buildSlotGroups, decodeSel, encodeSel, type SlotOptionGroup } from "../modelSlots";
import { ModelRefSelect } from "./ModelRefSelect";
import { sanitizeChain, type ModelSwitchView } from "./modelSwitch";

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
  const [modelOptions, setModelOptions] = useState<ModelOption[]>([]);
  const [ms, setMs] = useState<ModelSwitchView | null>(null);
  const [msErr, setMsErr] = useState<string | null>(null);

  const refresh = useCallback(() => {
    invoke<[boolean, string | null]>("nudge_status")
      .then(([, m]) => setBrain(m))
      .catch(() => {});
    invoke<ModelSlotsView>("model_slots_get")
      .then(setSlots)
      .catch(() => {});
    invoke<ModelSwitchView>("model_switch_get")
      .then(setMs)
      .catch((e) => setMsErr(String(e)));
    Promise.all([
      invoke<ModelOption[]>("list_models").catch(() => [] as ModelOption[]),
      invoke<DetectedLocalView[]>("probe_local_providers").catch(() => [] as DetectedLocalView[]),
    ]).then(([reg, local]) => {
      setModelOptions(reg);
      setGroups(buildSlotGroups(reg, local));
    });
  }, []);

  const saveMs = useCallback((next: ModelSwitchView) => {
    invoke<ModelSwitchView>("model_switch_set", { next })
      .then((saved) => {
        setMs(saved);
        setMsErr(null);
      })
      .catch((e) => setMsErr(String(e)));
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

  const addChainRow = () => {
    if (!ms) return;
    const pick =
      modelOptions.find((o) => !ms.fallback_chain.includes(o.ref_name))?.ref_name ?? modelOptions[0]?.ref_name;
    if (!pick) return;
    saveMs({ ...ms, fallback_chain: sanitizeChain([...ms.fallback_chain, pick]) });
  };

  const removeChainRow = (i: number) => {
    if (!ms) return;
    saveMs({ ...ms, fallback_chain: ms.fallback_chain.filter((_, idx) => idx !== i) });
  };

  const updateChainRow = (i: number, val: string | null) => {
    if (!ms || !val) return;
    const next = ms.fallback_chain.map((r, idx) => (idx === i ? val : r));
    saveMs({ ...ms, fallback_chain: sanitizeChain(next) });
  };

  const moveChain = (i: number, dir: -1 | 1) => {
    if (!ms) return;
    const j = i + dir;
    if (j < 0 || j >= ms.fallback_chain.length) return;
    const next = [...ms.fallback_chain];
    [next[i], next[j]] = [next[j], next[i]];
    saveMs({ ...ms, fallback_chain: next });
  };

  const setRouting = (patch: Partial<ModelSwitchView["routing"]>) => {
    if (!ms) return;
    saveMs({ ...ms, routing: { ...ms.routing, ...patch } });
  };

  return (
    <>
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

    {ms && (
      <section className="settings-section">
        <h3 className="settings-section__title">{t("settings.modelSwitch.title")}</h3>

        <div className="settings-row">
          <span className="settings-row__label">{t("settings.modelSwitch.default")}</span>
          <ModelRefSelect
            value={ms.default}
            options={modelOptions}
            allowEmpty
            ariaLabel={t("settings.modelSwitch.default")}
            onChange={(v) => saveMs({ ...ms, default: v })}
          />
        </div>
        <p className="settings-hint">{t("settings.modelSwitch.defaultHint")}</p>

        <div className="settings-row">
          <span className="settings-row__label">{t("settings.modelSwitch.chain")}</span>
          <button className="toolbar-btn" onClick={addChainRow} disabled={modelOptions.length === 0}>
            {t("settings.modelSwitch.chainAdd")}
          </button>
        </div>
        <p className="settings-hint">{t("settings.modelSwitch.chainHint")}</p>
        {ms.fallback_chain.length === 0 ? (
          <p className="settings-hint">{t("settings.modelSwitch.chainEmpty")}</p>
        ) : (
          ms.fallback_chain.map((ref, i) => (
            <div className="settings-row" key={`${ref}-${i}`}>
              <span className="settings-row__label">{i + 1}.</span>
              <ModelRefSelect
                value={ref}
                options={modelOptions}
                ariaLabel={`${t("settings.modelSwitch.chain")} ${i + 1}`}
                onChange={(v) => updateChainRow(i, v)}
              />
              <button
                className="toolbar-btn"
                onClick={() => moveChain(i, -1)}
                disabled={i === 0}
                aria-label={t("settings.modelSwitch.chainUp")}
              >
                ↑
              </button>
              <button
                className="toolbar-btn"
                onClick={() => moveChain(i, 1)}
                disabled={i === ms.fallback_chain.length - 1}
                aria-label={t("settings.modelSwitch.chainDown")}
              >
                ↓
              </button>
              <button
                className="toolbar-btn"
                onClick={() => removeChainRow(i)}
                aria-label={t("settings.modelSwitch.chainRemove")}
              >
                ✕
              </button>
            </div>
          ))
        )}

        <div className="settings-row">
          <label className="settings-row__label" htmlFor="ms-routing-enable">
            {t("settings.modelSwitch.routingEnable")}
          </label>
          <input
            id="ms-routing-enable"
            type="checkbox"
            checked={ms.routing.enabled}
            onChange={(e) => setRouting({ enabled: e.target.checked })}
          />
        </div>
        <p className="settings-hint">{t("settings.modelSwitch.routingHint")}</p>

        {ms.routing.enabled && (
          <>
            <div className="settings-row">
              <span className="settings-row__label">{t("settings.modelSwitch.cheap")}</span>
              <ModelRefSelect
                value={ms.routing.cheap}
                options={modelOptions}
                allowEmpty
                ariaLabel={t("settings.modelSwitch.cheap")}
                onChange={(v) => setRouting({ cheap: v })}
              />
            </div>
            <div className="settings-row">
              <span className="settings-row__label">{t("settings.modelSwitch.frontier")}</span>
              <ModelRefSelect
                value={ms.routing.frontier}
                options={modelOptions}
                allowEmpty
                ariaLabel={t("settings.modelSwitch.frontier")}
                onChange={(v) => setRouting({ frontier: v })}
              />
            </div>
            <div className="settings-row">
              <label className="settings-row__label" htmlFor="ms-routing-threshold">
                {t("settings.modelSwitch.threshold")}
              </label>
              <input
                id="ms-routing-threshold"
                className="input"
                type="number"
                min={0}
                value={ms.routing.threshold_input_tokens ?? ""}
                onChange={(e) => {
                  const raw = e.target.value;
                  setRouting({ threshold_input_tokens: raw === "" ? null : Number(raw) });
                }}
              />
            </div>
            <p className="settings-hint">{t("settings.modelSwitch.thresholdHint")}</p>
          </>
        )}

        {msErr && <p className="settings-hint slot-error">{msErr}</p>}
      </section>
    )}
    </>
  );
}
