//! Body of the browser card: rows, the login hint, the port line, the result.

import { useState } from "react";
import { useT } from "../../i18n";
import type { AnsweredView, CardView, OfferView, PickArg, PortStepView, RowView } from "./browserApi";

interface Props {
  card: CardView;
  busy: boolean;
  loggingIn: boolean;
  result: AnsweredView | null;
  note: string | null;
  onAnswer: (p: PickArg) => void;
  onLoggedIn: () => void;
}

function PortCommand({ cmd }: { cmd: string }) {
  const { t } = useT();
  const [copied, setCopied] = useState(false);
  return (
    <li className="perm__row">
      <code className="ssh-card__cmd">{cmd}</code>
      <button
        type="button"
        className="btn btn--sm btn--secondary"
        onClick={() =>
          void navigator.clipboard.writeText(cmd).then(() => setCopied(true)).catch(() => {})
        }
      >
        {copied ? t("browser.port.copied") : t("browser.port.copy")}
      </button>
    </li>
  );
}

/** D1: the port opens only from the CLI. The warning stays outside the
 *  copied text, so each copy button copies a runnable command. */
function PortStep({ step }: { step: PortStepView }) {
  return (
    <>
      <p className="field-muted">{step.warning}</p>
      <ul className="perm__list">
        {step.commands.map((c) => (
          <PortCommand key={c} cmd={c} />
        ))}
      </ul>
    </>
  );
}

type Translate = ReturnType<typeof useT>["t"];
type ReplaceOffer = Extract<OfferView, { kind: "replace_only" }>;

/** What a replace does, for the row and the login line alike: both answers
 *  approve the offer's `adds` (#2512), so both name them. No empty label. */
function replaceText(
  t: Translate,
  dropsKey: "browser.row.replaceDrops" | "browser.login.replacesBinding",
  o: ReplaceOffer,
): string {
  const drops = t(dropsKey, { drops: o.drops.join(", ") });
  return o.adds.length > 0 ? `${drops} ${t("browser.row.replaceAdds", { adds: o.adds.join(", ") })}` : drops;
}

function Row({
  row,
  busy,
  canPick,
  onAnswer,
}: {
  row: RowView;
  busy: boolean;
  /** False while the port is closed: the CLI line is the only pick (D1). */
  canPick: boolean;
  onAnswer: (p: PickArg) => void;
}) {
  const { t } = useT();
  const o: OfferView = row.offer;
  const detail =
    o.kind === "adds_hosts"
      ? t("browser.row.addsHosts", { hosts: o.missing.join(", ") })
      : o.kind === "replace_only"
        ? replaceText(t, "browser.row.replaceDrops", o)
        : o.kind === "disabled"
          ? o.why
          : null;
  return (
    <li className="perm__row browser-card__row">
      <strong>{row.name}</strong> <span className="field-muted">{row.domains.join(", ")}</span>
      {row.shared_with.length > 0 && (
        <span className="field-muted"> · {t("browser.row.sharedWith", { agents: row.shared_with.join(", ") })}</span>
      )}
      {detail && <div className="field-muted">{detail}</div>}
      {row.session_elsewhere && (
        <div className="field-muted">
          {t("browser.row.sessionElsewhere", { at: row.session_elsewhere })}
        </div>
      )}
      {o.kind === "already_bound" && <span className="field-muted"> {t("browser.row.bound")}</span>}
      {canPick && (o.kind === "offered" || o.kind === "adds_hosts" || o.kind === "already_bound") && (
        <button
          type="button"
          className="btn btn--sm btn--primary"
          disabled={busy}
          onClick={() => onAnswer({ kind: "row", name: row.name, replace: false })}
        >
          {t("browser.row.use")}
        </button>
      )}
      {canPick && o.kind === "replace_only" && (
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
  // Answering here never opens the port (D1): it would settle the request
  // with the site still unreachable. The port step's CLI command is the way.
  const portClosed = card.port_step !== null;
  return (
    <>
      {b.kind === "no_live_entry" && <p className="field-muted">{t("browser.noLiveEntry")}</p>}
      {b.kind === "login" && !finished && (
        <>
          <ul className="perm__list">
            {b.rows.map((r) => (
              <Row key={r.name} row={r} busy={busy} canPick={!portClosed} onAnswer={onAnswer} />
            ))}
          </ul>
          {loggingIn ? (
            <>
              <p className="field-muted">{t("browser.login.hint")}</p>
              <button type="button" className="btn btn--primary" onClick={onLoggedIn}>
                {t("browser.login.loggedIn")}
              </button>
            </>
          ) : b.login_offer.kind === "disabled" ? (
            <p className="field-muted">{b.login_offer.why}</p>
          ) : portClosed ? (
            // Saving a login would settle the request with the page still
            // unreachable; the port step's `--login` line does both.
            <p className="field-muted">{t("browser.login.portClosed")}</p>
          ) : (
            <>
              {b.login_offer.kind === "replace_only" && (
                <div className="field-muted">
                  {replaceText(t, "browser.login.replacesBinding", b.login_offer)}
                </div>
              )}
              <button
                type="button"
                className="btn btn--secondary browser-card__btn--wrap"
                disabled={busy}
                onClick={() => {
                  // `reauth` overwrites the saved login of that name;
                  // `replace` swaps the bound profile (#2461). Separate.
                  const rebinds = b.login_offer.kind === "replace_only";
                  if ((b.login_again_exists || rebinds) && !confirm) return setConfirm(true);
                  setConfirm(false);
                  onAnswer({ kind: "login", reauth: b.login_again_exists, replace: rebinds });
                }}
              >
                {confirm
                  ? t("browser.login.confirmReplace")
                  : t(b.login_again_exists ? "browser.login.againReplace" : "browser.login.again", {
                      name: b.login_again,
                    })}
              </button>
            </>
          )}
        </>
      )}
      {b.kind === "reach" && !finished && (
        <>
          {b.reach.kind === "allowed" && (
            <p className="field-muted">
              {t(portClosed ? "browser.reach.allowedPortClosed" : "browser.reach.allowed")}
            </p>
          )}
          {b.reach.kind === "unscoped" && (
            <p className="field-muted">{t("browser.reach.unscoped", { mode: b.reach.mode })}</p>
          )}
          {!portClosed && (b.reach.kind === "allowed" || b.reach.kind === "unscoped") && (
            <button
              type="button"
              className="btn btn--primary"
              disabled={busy}
              onClick={() => onAnswer({ kind: "allow" })}
            >
              {t("browser.reach.continue")}
            </button>
          )}
          {b.reach.kind === "disabled" && <p className="field-muted">{b.reach.why}</p>}
          {!portClosed && b.reach.kind === "needs_host" && (
            <button
              type="button"
              className="btn btn--primary browser-card__btn--wrap"
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
      {!finished && card.port_step && <PortStep step={card.port_step} />}
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
