//! Body of the browser card: rows, the login hint, the port line, the result.

import { useState } from "react";
import { useT } from "../../i18n";
import type { AnsweredView, CardView, OfferView, PickArg, RowView } from "./browserApi";

interface Props {
  card: CardView;
  busy: boolean;
  loggingIn: boolean;
  result: AnsweredView | null;
  note: string | null;
  onAnswer: (p: PickArg) => void;
  onLoggedIn: () => void;
}

function PortLine({ line }: { line: string }) {
  const { t } = useT();
  const [copied, setCopied] = useState(false);
  return (
    <>
      <code className="ssh-card__cmd">{line}</code>
      <button
        type="button"
        className="btn btn--sm btn--secondary"
        onClick={() =>
          void navigator.clipboard.writeText(line).then(() => setCopied(true)).catch(() => {})
        }
      >
        {copied ? t("browser.port.copied") : t("browser.port.copy")}
      </button>
    </>
  );
}

function Row({ row, busy, onAnswer }: { row: RowView; busy: boolean; onAnswer: (p: PickArg) => void }) {
  const { t } = useT();
  const o: OfferView = row.offer;
  const detail =
    o.kind === "adds_hosts"
      ? t("browser.row.addsHosts", { hosts: o.missing.join(", ") })
      : o.kind === "replace_only"
        ? `${t("browser.row.replaceDrops", { drops: o.drops.join(", ") })} ${t("browser.row.replaceAdds", { adds: o.adds.join(", ") })}`
        : o.kind === "disabled"
          ? o.why
          : null;
  return (
    <li className="perm__row">
      <strong>{row.name}</strong> <span className="field-muted">{row.domains.join(", ")}</span>
      {row.shared_with.length > 0 && (
        <span className="field-muted"> · {t("browser.row.sharedWith", { agents: row.shared_with.join(", ") })}</span>
      )}
      {detail && <div className="field-muted">{detail}</div>}
      {o.kind === "already_bound" && <span className="field-muted"> {t("browser.row.bound")}</span>}
      {(o.kind === "offered" || o.kind === "adds_hosts") && (
        <button
          type="button"
          className="btn btn--sm btn--primary"
          disabled={busy}
          onClick={() => onAnswer({ kind: "row", name: row.name, replace: false })}
        >
          {t("browser.row.use")}
        </button>
      )}
      {o.kind === "replace_only" && (
        <button
          type="button"
          className="btn btn--sm btn--secondary"
          disabled={busy}
          onClick={() => onAnswer({ kind: "row", name: row.name, replace: true })}
        >
          {t("browser.row.replace")}
        </button>
      )}
    </li>
  );
}

export function BrowserCardBody({ card, busy, loggingIn, result, note, onAnswer, onLoggedIn }: Props) {
  const { t } = useT();
  const [confirm, setConfirm] = useState(false);
  const b = card.body;
  const finished = result?.state === "done";
  return (
    <>
      {b.kind === "no_live_entry" && <p className="field-muted">{t("browser.noLiveEntry")}</p>}
      {b.kind === "login" && !finished && (
        <>
          <ul className="perm__list">
            {b.rows.map((r) => (
              <Row key={r.name} row={r} busy={busy} onAnswer={onAnswer} />
            ))}
          </ul>
          {loggingIn ? (
            <>
              <p className="field-muted">{t("browser.login.hint")}</p>
              <button type="button" className="btn btn--primary" onClick={onLoggedIn}>
                {t("browser.login.loggedIn")}
              </button>
            </>
          ) : (
            <button
              type="button"
              className="btn btn--secondary"
              disabled={busy}
              onClick={() => {
                if (b.login_again_exists && !confirm) return setConfirm(true);
                setConfirm(false);
                onAnswer({ kind: "login", reauth: b.login_again_exists, replace: b.login_again_exists });
              }}
            >
              {confirm
                ? t("browser.login.confirmReplace")
                : t(b.login_again_exists ? "browser.login.againReplace" : "browser.login.again", {
                    name: b.login_again,
                  })}
            </button>
          )}
        </>
      )}
      {b.kind === "reach" && !finished && (
        <>
          {b.reach.kind === "allowed" && <p className="field-muted">{t("browser.reach.allowed")}</p>}
          {b.reach.kind === "unscoped" && (
            <p className="field-muted">{t("browser.reach.unscoped", { mode: b.reach.mode })}</p>
          )}
          {b.reach.kind === "disabled" && <p className="field-muted">{b.reach.why}</p>}
          {b.reach.kind === "needs_host" && (
            <button
              type="button"
              className="btn btn--primary"
              disabled={busy}
              onClick={() => onAnswer({ kind: "allow" })}
            >
              {t("browser.reach.needsHost", { hosts: b.reach.missing.join(", ") })}
            </button>
          )}
        </>
      )}
      {!finished && !loggingIn && (
        <button
          type="button"
          className="btn btn--sm btn--secondary"
          disabled={busy}
          onClick={() => onAnswer({ kind: "dismiss" })}
        >
          {t("browser.dismiss")}
        </button>
      )}
      {card.port_line && <PortLine line={card.port_line} />}
      {result && (
        <>
          <ul className="perm__list">
            {result.lines.map((l) => (
              <li key={l} className="perm__row">{l}</li>
            ))}
          </ul>
          {result.state !== "done" && <p className="save-error">{t(`browser.state.${result.state}`)}</p>}
          {result.saved_as && result.status && (
            <p className="field-muted">
              {result.saved_as}:{" "}
              {t(`browser.status.${result.status}`, { at: result.reported_at ?? "" })}
            </p>
          )}
        </>
      )}
      {note && <p className="field-muted">{note}</p>}
    </>
  );
}
