import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

import { useT } from "../../../i18n";
import {
  scheduleExpr,
  scheduleNext,
  scheduleScope,
  type ScheduleItem,
  type ScheduleStatus,
} from "../../../schedule";

// Read-only by design, not by omission.
//
// The CLI is the complete surface for schedules (`mur agent schedule`,
// `mur fleet`, `mur workflow`); this answers "what is set up and when does it
// next fire" without duplicating the editing. A view that says where to edit is
// finished; one that offers a control it cannot honour is not.
export function ScheduleTab({ agentName }: { agentName: string }) {
  const { t } = useT();
  const [status, setStatus] = useState<ScheduleStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    invoke<ScheduleStatus>("panel_schedule_status", { agent: agentName })
      .then((s) => live && setStatus(s))
      .catch((e) => live && setError(String(e)));
    return () => {
      live = false;
    };
  }, [agentName]);

  if (error) return <p className="field-muted">{error}</p>;
  if (!status) return <p className="field-muted">{t("schedule.loading")}</p>;

  // Fleet and workflow schedules are machine-wide. They arrive with the agent's
  // own rows and, shown unlabelled, make a list of other people's schedules
  // look like this agent's — so they are separated rather than interleaved.
  const own = status.schedules.filter((s) => scheduleScope(s) === "agent");
  const global = status.schedules.filter((s) => scheduleScope(s) === "global");

  return (
    <div className="tab-form">
      <label className="field-label">{t("schedule.own")}</label>
      {own.length === 0 ? (
        <p className="field-muted" style={{ fontSize: 12 }}>
          {t("schedule.noneOwn")}
        </p>
      ) : (
        own.map((s, i) => <Row key={`own-${i}`} item={s} />)
      )}

      {global.length > 0 && (
        <>
          <label className="field-label" style={{ marginTop: 16 }}>
            {t("schedule.global")}
          </label>
          {global.map((s, i) => (
            <Row key={`all-${i}`} item={s} />
          ))}
        </>
      )}

      {status.warnings.map((w, i) => (
        <p key={`warn-${i}`} className="field-muted" style={{ fontSize: 11 }}>
          {w}
        </p>
      ))}

      <p
        className="field-muted"
        style={{ marginTop: 24, fontSize: 11, fontStyle: "italic" }}
      >
        {t("schedule.editHint")}
      </p>
    </div>
  );
}

function Row({ item }: { item: ScheduleItem }) {
  const next = scheduleNext(item);
  return (
    <div style={{ marginBottom: 10 }}>
      <div style={{ fontSize: 13 }}>
        <strong>{item.owner}</strong>{" "}
        <span title={scheduleExpr(item)}>{item.description}</span>
      </div>
      <div
        className={next.muted ? "field-muted" : undefined}
        style={{ fontSize: 12 }}
        title={next.title}
      >
        {next.text}
      </div>
    </div>
  );
}
