import { useState } from "react";
import { useT } from "../../../i18n";
import { OverrideDialog } from "./OverrideDialog";

/** Expanded content: guidance, plus the entry button while running. */
export function DangerZoneBody({ isRunning, onOpen }: { agentName: string; isRunning: boolean; onOpen: () => void }) {
  const { t } = useT();
  return (
    <div className="danger-zone__body">
      <p className="danger-zone__guidance">{t("override.zone.guidance")}</p>
      {isRunning ? (
        <button type="button" className="btn btn--danger" onClick={onOpen}>
          {t("override.zone.button")}
        </button>
      ) : (
        <p className="field-muted">{t("override.zone.stopped")}</p>
      )}
    </div>
  );
}

/** Dashboard-only, collapsed on every visit (expansion is never persisted). */
export function DangerZone({
  agentName,
  isRunning,
  dashboardOnly,
}: {
  agentName: string;
  isRunning: boolean;
  dashboardOnly?: boolean;
}) {
  const { t } = useT();
  const [open, setOpen] = useState(false);
  const [dialog, setDialog] = useState(false);
  if (!dashboardOnly) return null;
  return (
    <section className="detail-section danger-zone" id="agent-danger-zone">
      <button
        type="button"
        className="danger-zone__header"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      >
        {t("override.zone.header")}
      </button>
      {open && <DangerZoneBody agentName={agentName} isRunning={isRunning} onOpen={() => setDialog(true)} />}
      {dialog && <OverrideDialog agentName={agentName} onClose={() => setDialog(false)} />}
    </section>
  );
}
