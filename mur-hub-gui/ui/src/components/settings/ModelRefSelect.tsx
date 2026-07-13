import type { ModelOption } from "../modelPicker";
import { useT } from "../../i18n";

export interface ModelRefSelectProps {
  value: string | null;
  options: ModelOption[];
  onChange: (next: string | null) => void;
  allowEmpty?: boolean;
  /** Overrides the default "pick a model" text for the empty option (e.g. an "auto" hint). */
  emptyLabel?: string;
  ariaLabel?: string;
  disabled?: boolean;
}

/**
 * Presentational value/onChange picker over a flat `ref_name` — NOT agent-bound
 * (unlike `ModelCombobox`). Shared by the default/cheap/frontier/smart pickers
 * and each fallback-chain row.
 */
export function ModelRefSelect({ value, options, onChange, allowEmpty, emptyLabel, ariaLabel, disabled }: ModelRefSelectProps) {
  const { t } = useT();
  return (
    <select
      className="slot-select"
      value={value ?? ""}
      disabled={disabled}
      aria-label={ariaLabel}
      onChange={(e) => onChange(e.target.value || null)}
    >
      {allowEmpty && <option value="">{emptyLabel ?? t("settings.slots.pick")}</option>}
      {options.map((o) => (
        <option key={o.ref_name} value={o.ref_name}>
          {`${o.ref_name} (${o.provider}/${o.model})`}
        </option>
      ))}
    </select>
  );
}
