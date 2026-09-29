// The `propose` chip above the Hub composer (#1566). Rules live in
// `proposalChipModel.ts`; this is only rendering and the two side effects
// (clipboard copy, confirmed restart).
import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../i18n";
import { copyText, type ChipAction, type ChipState } from "./proposalChipModel";

interface Props {
  agentName: string;
  state: ChipState;
  dispatch: (a: ChipAction) => void;
}

export function ProposalChip({ agentName, state, dispatch }: Props) {
  const { t } = useT();
  const [copied, setCopied] = useState(false);
  if (state.phase === "none") return null;
  const { chip } = state;
  const text = copyText(chip);

  async function restart() {
    dispatch({ type: "restartStarted" });
    try {
      await invoke("restart_agent", { name: agentName });
      dispatch({ type: "restartDone" });
    } catch (e) {
      dispatch({ type: "restartFailed", error: String(e).split("\n")[0].slice(0, 200) });
    }
  }

  function copy() {
    if (!text) return;
    // `navigator.clipboard` needs a user gesture — this is one.
    navigator.clipboard
      .writeText(text)
      .then(() => setCopied(true))
      .catch(console.error);
  }

  return (
    <div className="proposal-chip" role="status" aria-live="polite">
      <span className="proposal-chip__mark" aria-hidden="true">⤷</span>
      {text && <code className="proposal-chip__cmd">{text}</code>}
      <span className="proposal-chip__label">{chip.label}</span>
      <span className="proposal-chip__actions">
        {text && (
          <button className="proposal-chip__btn" onClick={copy}>
            {copied ? t("proposal.copied") : t("proposal.copy")}
          </button>
        )}
        {chip.kind === "restart" && state.phase === "offered" && (
          <button className="proposal-chip__btn" onClick={() => dispatch({ type: "askConfirm" })}>
            {t("proposal.restart")}
          </button>
        )}
        {state.phase === "confirming" && (
          <>
            <span className="proposal-chip__confirm">{t("proposal.restartConfirm", { name: agentName })}</span>
            <button className="proposal-chip__btn proposal-chip__btn--primary" onClick={() => void restart()}>
              {t("proposal.restartYes")}
            </button>
            <button className="proposal-chip__btn" onClick={() => dispatch({ type: "cancelConfirm" })}>
              {t("proposal.cancel")}
            </button>
          </>
        )}
        {state.phase === "restarting" && (
          <span className="proposal-chip__confirm">{t("proposal.restarting")}</span>
        )}
        {state.phase === "failed" && (
          <span className="proposal-chip__error">{t("proposal.restartFailed", { error: state.error })}</span>
        )}
        {state.phase !== "restarting" && (
          <button
            className="proposal-chip__dismiss"
            onClick={() => dispatch({ type: "dismiss" })}
            title={t("proposal.dismiss")}
            aria-label={t("proposal.dismiss")}
          >
            ×
          </button>
        )}
      </span>
    </div>
  );
}
