//! Permissions tab, SSH block (SSH setup spec, "Hub"): keys and their hosts
//! with the signing state, pending requests in the agent's own words, and
//! "Add host". Removing goes through `mur agent ssh revoke`.

import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { useT } from "../../i18n";
import { SSH_REQUEST_EVENT, type OverviewView, sshApi } from "./sshApi";
import { expiredMarkText, pairStateText } from "./sshModel";
import { SshCard, type SshTarget } from "./SshCard";

export function SshBlock({ agent }: { agent: string }) {
  const { t } = useT();
  const [ov, setOv] = useState<OverviewView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [card, setCard] = useState<SshTarget | null>(null);
  const [confirm, setConfirm] = useState<string | null>(null);
  const [host, setHost] = useState("");
  const [port, setPort] = useState("");
  const [user, setUser] = useState("");

  const load = useCallback(() => {
    sshApi
      .overview(agent)
      .then((o) => {
        setOv(o);
        setError(null);
      })
      .catch((e) => setError(String(e)));
  }, [agent]);

  useEffect(() => {
    load();
    const un = listen<string>(SSH_REQUEST_EVENT, (e) => {
      if (e.payload === agent) load();
    });
    return () => {
      void un.then((f) => f());
    };
  }, [agent, load]);

  function revoke(keyFp: string, hostKeyFp: string | null) {
    const id = `${keyFp}|${hostKeyFp ?? ""}`;
    if (confirm !== id) {
      setConfirm(id);
      return;
    }
    sshApi
      .revoke(agent, keyFp, hostKeyFp)
      .then((o) => {
        setOv(o);
        setConfirm(null);
      })
      .catch((e) => setError(String(e)));
  }

  function removeBtn(keyFp: string, hostKeyFp: string | null) {
    const armed = confirm === `${keyFp}|${hostKeyFp ?? ""}`;
    return (
      <button type="button" className="perm__row-x" onClick={() => revoke(keyFp, hostKeyFp)}>
        {armed ? t("ssh.removeConfirm") : t("ssh.remove")}
      </button>
    );
  }

  const open = ov?.pending.filter((p) => !p.granted) ?? [];

  return (
    <div className="ssh-block">
      {error && <p className="save-error">{error}</p>}
      {ov?.decisions_unverified && (
        <p className="perm__note perm__note--attention">{t("ssh.unverified", { why: ov.decisions_unverified })}</p>
      )}
      {ov && ov.keys.length === 0 && <p className="field-muted perm__muted">{t("ssh.none")}</p>}
      {ov && ov.keys.length > 0 && (
        <ul className="perm__list">
          {ov.keys.map((k) => (
            <li key={k.fingerprint}>
              <div className="perm__row">
                <code>{k.label ?? k.fingerprint}</code>
                {!k.loaded && <span className="perm__detail ssh-warn">{t("ssh.keyNotLoaded")}</span>}
                {removeBtn(k.fingerprint, null)}
              </div>
              <ul className="perm__list ssh-hosts">
                {k.hosts.map((h) => (
                  <li key={h.host_key}>
                    <div className="perm__row">
                      <code>{h.names.join(", ") || h.label || h.host_key}</code>
                      <span className="perm__detail">{pairStateText(h, t)}</span>
                      {removeBtn(k.fingerprint, h.host_key)}
                    </div>
                    {expiredMarkText(h, t) && <p className="perm__detail ssh-dim">{expiredMarkText(h, t)}</p>}
                  </li>
                ))}
              </ul>
            </li>
          ))}
        </ul>
      )}

      {open.length > 0 && (
        <>
          <p className="field-muted perm__muted">{t("ssh.pending.title")}</p>
          <ul className="perm__list">
            {open.map((p) => (
              <li key={p.canonical} className="ssh-pending">
                <div className="perm__row">
                  <code>{p.canonical}</code>
                  <button
                    type="button"
                    className="btn btn--sm btn--primary"
                    onClick={() => setCard({ agent, host: p.host, port: p.port, user: p.user })}
                  >
                    {t("ssh.setUp")}
                  </button>
                  <button
                    type="button"
                    className="btn btn--sm btn--secondary"
                    onClick={() => void sshApi.dismiss(agent, p.host, p.port).then(load).catch((e) => setError(String(e)))}
                  >
                    {t("ssh.dismiss")}
                  </button>
                </div>
                <blockquote className="ssh-reason">{p.reason}</blockquote>
              </li>
            ))}
          </ul>
        </>
      )}

      <div className="perm__add">
        <input className="input" placeholder={t("ssh.field.host")} value={host} onChange={(e) => setHost(e.target.value)} />
        <input className="input ssh-port" placeholder={t("ssh.field.port")} inputMode="numeric" value={port} onChange={(e) => setPort(e.target.value.replace(/\D/g, ""))} />
        <input className="input" placeholder={t("ssh.field.user")} value={user} onChange={(e) => setUser(e.target.value)} />
        <button
          type="button"
          className="btn btn--sm btn--secondary"
          disabled={!host.trim()}
          onClick={() => setCard({ agent, host: host.trim(), port: port ? Number(port) : null, user: user.trim() || null })}
        >
          {t("ssh.addHost")}
        </button>
      </div>

      {card && (
        <SshCard
          target={card}
          onClose={(changed) => {
            setCard(null);
            if (changed) load();
          }}
        />
      )}
    </div>
  );
}
