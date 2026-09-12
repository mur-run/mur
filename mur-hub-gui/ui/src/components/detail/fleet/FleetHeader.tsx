import { useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import type { FleetDetail as Detail } from "../../fleet/types";
import { useT } from "../../../i18n";
import { modeBadgeLabel } from "../../fleet/fleetSettingsForm";
import { SplitButton } from "../../shell/SplitButton";
import { OverflowMenu } from "../../shell/OverflowMenu";
import { deleteFleet, showToast, useFleetCall } from "./fleetActions";

type T = ReturnType<typeof useT>["t"];

/** The meta line under the title: router · members · channel · mode. */
export function fleetMeta(detail: Detail, t: T): ReactNode {
  const mode = modeBadgeLabel(detail.parallel_summary, t);
  return (
    <>
      <span>{t("fleet.router")}: {detail.router}</span>
      <span className="sep">·</span>
      <span>{t("fleet.rowSubtitle", { count: detail.members.length })}</span>
      <span className="sep">·</span>
      <span className="mono">{detail.channel_id}</span>
      {mode && (
        <>
          <span className="sep">·</span>
          <span className="fleet-detail__mode-badge">{mode}</span>
        </>
      )}
    </>
  );
}

export interface FleetHeaderProps {
  detail: Detail;
  onRefresh: () => void;
  onDelete: () => void;
  /** Dashboard only: the ⋯ "Open in window" item. Undefined inside a window. */
  onOpenInWindow?: () => void;
}

/** Header actions (spec §4.4): Run ▾ (loop / send job / worktree), Stop or
 *  Start, and ⋯ (Export, Import, Delete). Rendered into DetailPage's
 *  `actions` slot; the loop and send-job forms open as popovers below it. */
export function FleetHeader({ detail, onRefresh, onDelete, onOpenInWindow }: FleetHeaderProps) {
  const { t } = useT();
  const { busy, setBusy, call } = useFleetCall(onRefresh);
  const [worktree, setWorktree] = useState(false);
  const [sendOpen, setSendOpen] = useState(false);
  const [sendInput, setSendInput] = useState("");

  // No override fields any more (spec §10.1): the loop's bounds live in the
  // Settings tab's LimitsPanel, one scope, always in force -- there is
  // nothing left for a per-run popover to collect.
  async function handleRunLoop() {
    showToast(t("fleet.runStarted"));
    await call("fleet_run_loop", { name: detail.name });
  }

  async function handleRun() {
    showToast(t("fleet.runStarted"));
    await call("fleet_run", { name: detail.name, worktree });
  }

  async function handleSend() {
    const text = sendInput.trim();
    if (!text) return;
    setBusy("fleet_send");
    try {
      const jobId = await invoke<string>("fleet_send", { name: detail.name, text });
      setSendInput("");
      setSendOpen(false);
      showToast(`Job queued: ${jobId.slice(0, 8)}`);
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  async function handleExport() {
    const dest = await save({
      defaultPath: `${detail.name}.fleet`,
      filters: [{ name: "Fleet Bundle", extensions: ["fleet"] }],
    });
    if (!dest) return;
    setBusy("fleet_export");
    try {
      await invoke("fleet_export_to", { name: detail.name, path: dest });
      showToast(t("fleet.exported").replace("{path}", dest), 4000);
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  async function handleImport() {
    const selected = await open({ filters: [{ name: "Fleet", extensions: ["fleet"] }] });
    if (!selected) return;
    const filePath = typeof selected === "string" ? selected : selected[0];
    if (!filePath) return;
    setBusy("fleet_import");
    try {
      const name = await invoke<string>("fleet_import", { path: filePath });
      showToast(t("fleet.imported").replace("{name}", name));
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }

  function handleDelete() {
    void deleteFleet(
      detail,
      { confirm: t("fleet.confirmDelete").replace("{name}", detail.display_name), title: t("fleet.delete") },
      setBusy,
      onDelete,
    );
  }

  const disabled = busy !== null;
  const stopped = detail.stopped;
  const startStopClass = stopped ? "btn btn--primary" : "btn btn--secondary";

  return (
    <>
      <SplitButton
        label={<>▶ {t("fleet.run")}</>}
        onPrimary={() => {
          void handleRun();
        }}
        disabled={disabled || stopped}
        menuLabel={t("fleet.runOptions")}
        items={[
          {
            id: "loop",
            label: t("fleet.run.loop"),
            onSelect: () => {
              void handleRunLoop();
            },
          },
          {
            id: "send",
            label: t("fleet.send"),
            onSelect: () => setSendOpen(true),
          },
          ...(detail.parallel_summary
            ? [
                {
                  id: "worktree",
                  label: worktree ? t("fleet.run.worktreeOn") : t("fleet.run.worktreeOff"),
                  onSelect: () => setWorktree((v) => !v),
                },
              ]
            : []),
        ]}
      />
      {stopped ? (
        <button type="button" className={startStopClass} onClick={() => call("fleet_start", { name: detail.name })} disabled={disabled}>
          {t("fleet.start")}
        </button>
      ) : (
        <button type="button" className={startStopClass} onClick={() => call("fleet_stop", { name: detail.name })} disabled={disabled}>
          {t("fleet.stop")}
        </button>
      )}
      <OverflowMenu
        label={t("action.more")}
        items={[
          { id: "export", label: t("fleet.export"), onSelect: () => { void handleExport(); } },
          { id: "import", label: t("fleet.import"), onSelect: () => { void handleImport(); } },
          ...(onOpenInWindow
            ? [{ id: "openInWindow", label: t("action.openInWindow"), onSelect: onOpenInWindow }]
            : []),
          { id: "delete", label: t("fleet.delete"), danger: true, onSelect: handleDelete },
        ]}
      />
      {sendOpen && (
        <div className="fleet-popover" role="dialog" aria-label={t("fleet.send")}>
          <div className="fleet-send">
            <input
              autoFocus
              value={sendInput}
              onChange={(e) => setSendInput(e.target.value)}
              placeholder={t("fleet.sendPlaceholder")}
              onKeyDown={(e) => {
                if (e.key === "Enter") void handleSend();
                if (e.key === "Escape") setSendOpen(false);
              }}
            />
            <button
              type="button"
              className="btn btn--primary"
              onClick={() => {
                void handleSend();
              }}
              disabled={disabled || !sendInput.trim()}
            >
              {t("fleet.send")}
            </button>
            <button type="button" className="btn btn--secondary" onClick={() => setSendOpen(false)}>
              {t("fleet.cancel")}
            </button>
          </div>
        </div>
      )}
    </>
  );
}
