// Shared ordered-list-of-ModelRefSelect editor for a fallback chain.
// Used by both the global model-switch settings (ModelsSettings.tsx) and the
// per-agent fallback override (AgentInspector.tsx) — extracted here to keep
// the add/remove/reorder row logic in one place (DRY across Tasks 3 and 4).
import type { ModelOption } from "../modelPicker";
import { useT } from "../../i18n";
import type { TranslationKey } from "../../i18n/types";
import { ModelRefSelect } from "./ModelRefSelect";
import { sanitizeChain } from "./modelSwitch";

export interface FallbackChainEditorProps {
  chain: string[];
  options: ModelOption[];
  onChange: (next: string[]) => void;
  /** i18n key for the empty-chain hint. Defaults to the global "no fallback
   *  models configured" copy; callers with a different empty-state meaning
   *  (e.g. "inherits global chain") pass their own key. */
  emptyHintKey?: TranslationKey;
  disabled?: boolean;
}

export function FallbackChainEditor({
  chain,
  options,
  onChange,
  emptyHintKey = "settings.modelSwitch.chainEmpty",
  disabled,
}: FallbackChainEditorProps) {
  const { t } = useT();

  const addRow = () => {
    const pick = options.find((o) => !chain.includes(o.ref_name))?.ref_name ?? options[0]?.ref_name;
    if (!pick) return;
    onChange(sanitizeChain([...chain, pick]));
  };

  const removeRow = (i: number) => {
    onChange(chain.filter((_, idx) => idx !== i));
  };

  const updateRow = (i: number, val: string | null) => {
    if (!val) return;
    onChange(sanitizeChain(chain.map((r, idx) => (idx === i ? val : r))));
  };

  const moveRow = (i: number, dir: -1 | 1) => {
    const j = i + dir;
    if (j < 0 || j >= chain.length) return;
    const next = [...chain];
    [next[i], next[j]] = [next[j], next[i]];
    onChange(next);
  };

  return (
    <>
      <div className="settings-row">
        <span className="settings-row__label">{t("settings.modelSwitch.chain")}</span>
        <button className="toolbar-btn" onClick={addRow} disabled={disabled || options.length === 0}>
          {t("settings.modelSwitch.chainAdd")}
        </button>
      </div>
      {chain.length === 0 ? (
        <p className="settings-hint">{t(emptyHintKey)}</p>
      ) : (
        chain.map((ref, i) => (
          <div className="settings-row" key={`${ref}-${i}`}>
            <span className="settings-row__label">{i + 1}.</span>
            <ModelRefSelect
              value={ref}
              options={options}
              ariaLabel={`${t("settings.modelSwitch.chain")} ${i + 1}`}
              disabled={disabled}
              onChange={(v) => updateRow(i, v)}
            />
            <button
              className="toolbar-btn"
              onClick={() => moveRow(i, -1)}
              disabled={disabled || i === 0}
              aria-label={t("settings.modelSwitch.chainUp")}
            >
              ↑
            </button>
            <button
              className="toolbar-btn"
              onClick={() => moveRow(i, 1)}
              disabled={disabled || i === chain.length - 1}
              aria-label={t("settings.modelSwitch.chainDown")}
            >
              ↓
            </button>
            <button
              className="toolbar-btn"
              onClick={() => removeRow(i)}
              disabled={disabled}
              aria-label={t("settings.modelSwitch.chainRemove")}
            >
              ✕
            </button>
          </div>
        ))
      )}
    </>
  );
}
