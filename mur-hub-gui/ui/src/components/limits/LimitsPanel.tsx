import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import type { LimitsRowView, LimitsView } from "../fleet/types";
import { chipLabel, patchForReset, patchForSave, rowAction, rowIsDimmed } from "./limitsPanelLogic";

export interface LimitsPanelProps {
  scope: LimitsView["scope"];
  name?: string;
  /** The fleet detail already carries one (spec §10.5: one network round
   *  trip, not two); global/agent scopes fetch their own. */
  initial?: LimitsView;
  focusKnob?: LimitsRowView["knob"];
  onChanged?: (v: LimitsView) => void;
}

/** One panel, three scopes (spec §10.1). Every value and every "applies"
 *  comes from `limits_resolve`; this component only decides which
 *  affordance to show — never a default, never an applicability rule. */
export function LimitsPanel({ scope, name, initial, focusKnob, onChanged }: LimitsPanelProps) {
  const { t } = useT();
  const [view, setView] = useState<LimitsView | null>(initial ?? null);
  const [editing, setEditing] = useState<LimitsRowView["knob"] | null>(focusKnob ?? null);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    if (initial) {
      setView(initial);
      return;
    }
    invoke<LimitsView>("limits_resolve", { scope, name: name ?? null })
      .then(setView)
      .catch((e) => setErr(String(e)));
  }, [scope, name, initial]);

  const apply = async (cmd: "limits_set" | "limits_remove_stale", args: Record<string, unknown>) => {
    setBusy(true);
    setErr(null);
    try {
      const v = await invoke<LimitsView>(cmd, { scope, name: name ?? null, ...args });
      setView(v);
      setEditing(null);
      onChanged?.(v);
    } catch (e) {
      // The CLI parser's own words, e.g. "limits.deadline: `soon` is not a duration".
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  if (!view) return <p className="overview-now__sub">{err ?? "…"}</p>;

  return (
    <div className="limits-panel">
      {view.error && (
        <div className="fleet-settings__warning limits-panel__error">
          {t("limits.error")}: {view.error}
        </div>
      )}
      {view.stale.map((s) => (
        <div key={s.key} className="limits-stale">
          <span>{t("limits.stale", { key: s.key, value: s.value, file: s.file })}</span>
          <button
            type="button"
            className="btn btn--link"
            disabled={busy}
            onClick={() => apply("limits_remove_stale", { key: s.key })}
          >
            {t("limits.remove")}
          </button>
        </div>
      ))}
      {view.rows.map((row) => {
        const action = rowAction(row);
        const isEditing = editing === row.knob;
        return (
          <div key={row.knob} className={`limits-row${rowIsDimmed(row) ? " limits-row--inherited" : ""}`}>
            <span className="limits-row__knob mono">{row.knob}</span>
            {action === "none" ? (
              <span className="limits-row__note">{row.note}</span>
            ) : isEditing ? (
              <>
                <input
                  autoFocus
                  value={draft}
                  onChange={(e) => setDraft(e.target.value)}
                  placeholder={row.knob === "cost_usd" ? "5" : row.knob === "stuck" ? "10m · off" : "2h"}
                />
                <button
                  type="button"
                  className="btn btn--primary"
                  disabled={busy || !draft.trim()}
                  onClick={() => apply("limits_set", { patch: patchForSave(row.knob, draft) })}
                >
                  {t("limits.save")}
                </button>
                <button type="button" className="btn btn--link" onClick={() => setEditing(null)}>
                  {t("limits.cancel")}
                </button>
              </>
            ) : (
              <>
                <b className="limits-row__value">{row.value}</b>
                <span className="limits-chip">{chipLabel(row, scope, t)}</span>
                {action === "reset" ? (
                  <button
                    type="button"
                    className="btn btn--link"
                    disabled={busy}
                    onClick={() => apply("limits_set", { patch: patchForReset(row.knob) })}
                  >
                    {t("limits.reset")}
                  </button>
                ) : (
                  <button
                    type="button"
                    className="btn btn--link"
                    disabled={busy}
                    onClick={() => {
                      setDraft(row.raw ?? "");
                      setEditing(row.knob);
                    }}
                  >
                    {t("limits.override")}
                  </button>
                )}
                {row.note && <span className="limits-row__note">{row.note}</span>}
              </>
            )}
          </div>
        );
      })}
      {err && <div className="fleet-settings__warning">{err}</div>}
      <p className="fleet-settings__hint">
        {view.attended_note}
        {view.needs_restart ? ` · ${t("limits.restartRequired")}` : ""}
      </p>
    </div>
  );
}
