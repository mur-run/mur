//! Fleet inspector — contextual right-pane for the Fleets page. Shows the
//! selected fleet's members and loop/schedule state, self-fetched from the
//! same `fleet_detail` query FleetView uses.

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { FleetDetail } from "../fleet/types";
import { useT } from "../../i18n";

interface Props {
  fleetName: string;
  onClose: () => void;
}

export function FleetInspector({ fleetName, onClose }: Props) {
  const { t } = useT();
  const [detail, setDetail] = useState<FleetDetail | null>(null);

  useEffect(() => {
    let cancelled = false;
    invoke<FleetDetail>("fleet_detail", { name: fleetName })
      .then((d) => { if (!cancelled) setDetail(d); })
      .catch(() => { if (!cancelled) setDetail(null); });
    return () => { cancelled = true; };
  }, [fleetName]);

  const loop = detail?.loop_cfg ?? null;

  return (
    <aside className="detail-panel detail-panel--inspector">
      <div className="detail-panel__header">
        <div className="detail-panel__top">
          <div className="detail-panel__ident">
            <div className="detail-panel__name">{detail?.display_name ?? fleetName}</div>
            <span className="field-muted" style={{ fontSize: 12 }}>{t("fleetInspector.subtitle")}</span>
          </div>
          <button
            className="detail-panel__close"
            onClick={onClose}
            title={t("detail.close")}
            aria-label={t("detail.close")}
          >
            ×
          </button>
        </div>
      </div>
      <div className="detail-panel__body">
        <div className="tab-form">
          {detail?.goal && (
            <>
              <label className="field-label">{t("fleetInspector.goal")}</label>
              <p className="field-value">{detail.goal}</p>
            </>
          )}

          <label className="field-label" style={{ marginTop: 12 }}>{t("fleetInspector.channel")}</label>
          <code className="item-card-code">{detail?.channel_id ?? `fleet-${fleetName}`}</code>

          <label className="field-label" style={{ marginTop: 12 }}>
            {t("fleetInspector.members", { count: detail?.members.length ?? 0 })}
          </label>
          {detail && detail.members.length > 0 ? (
            <ul className="item-list">
              {detail.router && (
                <li className="item-card">
                  <div className="item-card-name">{detail.router}</div>
                  <span className="badge-sm">{t("fleetInspector.router")}</span>
                </li>
              )}
              {detail.members
                .filter((m) => m !== detail.router)
                .map((m) => (
                  <li key={m} className="item-card">
                    <div className="item-card-name">{m}</div>
                  </li>
                ))}
            </ul>
          ) : (
            <p className="field-muted" style={{ fontSize: 12 }}>{t("fleetInspector.noMembers")}</p>
          )}

          <label className="field-label" style={{ marginTop: 12 }}>{t("fleetInspector.loop")}</label>
          {loop ? (
            <div className="tab-form" style={{ gap: 6 }}>
              <p className="field-muted" style={{ fontSize: 12 }}>
                {t("fleetInspector.trigger")}: <b>{loop.trigger || "—"}</b>
              </p>
              <p className="field-muted" style={{ fontSize: 12 }}>
                {t("fleetInspector.maxIterations")}: <b>{loop.max_iterations}</b>
              </p>
              <p className="field-muted" style={{ fontSize: 12 }}>
                {t("fleetInspector.budget")}: <b>${loop.budget_usd}</b>
              </p>
              {loop.done_when && (
                <p className="field-muted" style={{ fontSize: 12 }}>
                  {t("fleetInspector.doneWhen")}: <code>{loop.done_when}</code>
                </p>
              )}
              <p className="field-muted" style={{ fontSize: 12 }}>
                {t("fleetInspector.lastRun")}: <b>{loop.last_run ?? t("fleetInspector.never")}</b>
              </p>
            </div>
          ) : (
            <p className="field-muted" style={{ fontSize: 12 }}>{t("fleetInspector.noLoop")}</p>
          )}

          {detail?.stopped && (
            <p className="save-error" style={{ marginTop: 8 }}>{t("fleetInspector.stopped")}</p>
          )}
        </div>
      </div>
    </aside>
  );
}
