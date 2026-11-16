//! The SSH card's result pane, after Approve. Split out of `SshCard.tsx`
//! before slice 6c grows it.

import { useT } from "../../i18n";
import type { ApplyView } from "./sshApi";
import { resultState } from "./sshModel";

export function SshCardResult({ result, note }: { result: ApplyView; note: string | null }) {
  const { t } = useT();
  const state = resultState(result);
  const line = result.install.line;
  return (
    <>
      <ul className="perm__list">
        {result.lines.map((l) => (
          <li key={l} className="perm__row">{l}</li>
        ))}
      </ul>
      {state === "held" && <p className="save-error">{t("ssh.held.title")}</p>}
      {state === "manual" && <p className="field-label">{t("ssh.manual.title")}</p>}
      {state !== "closed" && line && (
        <>
          <p className="field-muted">{t("ssh.dedicated.lineIntro")}</p>
          <code className="ssh-card__cmd">{line}</code>
        </>
      )}
      {note && <p className="field-muted">{note}</p>}
    </>
  );
}
