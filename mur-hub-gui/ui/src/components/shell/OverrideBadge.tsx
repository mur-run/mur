import { useT } from "../../i18n";

/** Red marker for a runtime booted with startup overrides. Passive unless
 *  the caller (dashboard only) passes `onRestartNormally`. */
export function OverrideBadge({ flags, onRestartNormally }: { flags: string[]; onRestartNormally?: () => void }) {
  const { t } = useT();
  return (
    <span className="override-badge">
      <span className="badge badge--danger">{t("override.badge", { flags: flags.join(" ") })}</span>
      {onRestartNormally && (
        <button type="button" className="btn btn--secondary btn--sm" onClick={onRestartNormally}>
          {t("override.badge.restartNormally")}
        </button>
      )}
    </span>
  );
}
