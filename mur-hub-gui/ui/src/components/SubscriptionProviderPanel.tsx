/**
 * SubscriptionProviderPanel — connect a CLI-managed subscription (ChatGPT
 * via Codex, Claude via Claude Code).
 *
 * That CLI owns the login; the loopback gateway owns the token; this panel
 * only ever sees display-safe views. It never calls the generic
 * `test_provider` or `/v1/models` path — those expect an API key, and these
 * providers have none. What to render is decided by
 * `deriveSubscriptionState` (pure, tested); everything provider-specific
 * comes from the descriptor. This file is the wiring.
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ModelOption } from "./modelPicker";
import { useT } from "../i18n";
import type { TranslationKey } from "../i18n/types";
import {
  togglePick,
  type SubscriptionCopy,
  type SubscriptionDescriptor,
} from "./modelLibraryHelpers";
import {
  defaultChatGPTAlias,
  deriveSubscriptionState,
  gatewayProblem,
  type ChatGPTAccount,
  type ChatGPTModel,
  type ChatGPTPanelState,
  type GatewayProblem,
  type GatewayStatus,
} from "./chatgptSubscription";

interface LoginResult {
  authenticated: boolean;
  error?: string | null;
}

const NO_GATEWAY: GatewayStatus = {
  installed: false,
  running: false,
  codex_hook: false,
  credential_mode: null,
  compression: false,
};

const PROBLEM_KEY: Record<GatewayProblem, TranslationKey> = {
  "not-running": "lib.chatgpt.gateway.notRunning",
  "hook-missing": "lib.chatgpt.gateway.hookMissing",
  "credential-apikey": "lib.chatgpt.gateway.credentialApikey",
  "credential-missing": "lib.chatgpt.gateway.credentialMissing",
};

type Confirm = "install" | "logout" | null;

export function SubscriptionProviderPanel({
  descriptor,
  registryModels,
  onModelsAdded,
}: {
  descriptor: SubscriptionDescriptor;
  registryModels: ModelOption[];
  onModelsAdded: () => void;
}) {
  const { t } = useT();
  const [account, setAccount] = useState<ChatGPTAccount | null>(null);
  const [accountError, setAccountError] = useState<string | null>(null);
  const [gateway, setGateway] = useState<GatewayStatus | null>(null);
  const [models, setModels] = useState<ChatGPTModel[] | null>(null);
  const [modelsError, setModelsError] = useState<string | null>(null);
  const [loginInProgress, setLoginInProgress] = useState(false);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<Confirm>(null);
  const [picks, setPicks] = useState<Set<string>>(new Set());
  const [aliases, setAliases] = useState<Record<string, string>>({});
  const [manualId, setManualId] = useState("");
  const [tick, setTick] = useState(0);
  const refresh = () => setTick((n) => n + 1);

  // Account and gateway in parallel; one cancellation flag per effect.
  useEffect(() => {
    let cancelled = false;
    setAccountError(null);
    invoke<ChatGPTAccount>(descriptor.commands.accountRead)
      .then((a) => {
        if (!cancelled) setAccount(a);
      })
      .catch((e) => {
        if (cancelled) return;
        setAccount(null);
        setAccountError(String(e));
      });
    invoke<GatewayStatus>("chatgpt_gateway_status")
      .then((g) => {
        if (!cancelled) setGateway(g);
      })
      .catch(() => {
        if (!cancelled) setGateway(NO_GATEWAY);
      });
    return () => {
      cancelled = true;
    };
  }, [tick, descriptor]);

  // Models only for a ChatGPT login with a usable gateway.
  const loggedIn = account?.logged_in === true;
  const gatewayUsable = gateway !== null && gatewayProblem(gateway, descriptor.readiness) === null;
  useEffect(() => {
    if (!loggedIn || !gatewayUsable) return;
    let cancelled = false;
    setModels(null);
    setModelsError(null);
    invoke<ChatGPTModel[]>(descriptor.commands.modelsList)
      .then((ms) => {
        if (cancelled) return;
        setModels(ms);
        setPicks(new Set(ms.filter((m) => m.is_default).map((m) => m.id)));
      })
      .catch((e) => {
        if (cancelled) return;
        setModels(null);
        setModelsError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [loggedIn, gatewayUsable, tick, descriptor]);

  const state = deriveSubscriptionState(
    {
    account,
    accountError,
    loginInProgress,
      gateway,
      models,
      modelsError,
    },
    descriptor.readiness,
  );

  async function run(action: () => Promise<void>) {
    setBusy(true);
    setActionError(null);
    setNotice(null);
    try {
      await action();
    } catch (e) {
      setActionError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function login() {
    setLoginInProgress(true);
    setActionError(null);
    try {
      const r = await invoke<LoginResult>(descriptor.commands.login);
      if (!r.authenticated) {
        setActionError(t(descriptor.copy.loginFailed, { error: r.error ?? "" }));
      }
    } catch (e) {
      setActionError(t(descriptor.copy.loginFailed, { error: String(e) }));
    } finally {
      setLoginInProgress(false);
      refresh();
    }
  }

  const installGateway = () =>
    run(async () => {
      setConfirm(null);
      const g = await invoke<GatewayStatus>("chatgpt_gateway_install", { consented: true });
      setGateway(g);
      refresh();
    });

  const logout = () =>
    run(async () => {
      setConfirm(null);
      await invoke(descriptor.commands.logout, { confirmed: true });
      setModels(null);
      refresh();
    });

  const addPicks = (list: Array<{ model: string; alias: string; verified: boolean }>) =>
    run(async () => {
      await invoke(descriptor.commands.modelsAdd, { picks: list });
      setNotice(t("lib.addedOk", { count: String(list.length) }));
      setManualId("");
      onModelsAdded();
    });

  const disconnect = () =>
    run(async () => {
      const n = await invoke<number>(descriptor.commands.disconnect);
      setNotice(t("lib.chatgpt.disconnectedOk", { count: String(n) }));
      onModelsAdded();
    });

  const removeEntry = (ref: string) =>
    run(async () => {
      await invoke("remove_model", { refName: ref });
      onModelsAdded();
    });

  const inRegistry = registryModels.filter((m) => m.provider === descriptor.provider);
  const registrySet = new Set(inRegistry.map((m) => m.model));

  return (
    <div className="ml-chatgpt">
      <div className="ml-panel-h">
        <span
          className="ml-logo ml-logo--lg"
          style={{ background: descriptor.color }}
          aria-hidden="true"
        >
          {descriptor.logo}
        </span>
        <div>
          <h2 className="ml-panel-h__title">{t(descriptor.copy.name)}</h2>
          <div className="ml-panel-h__sub">{t(descriptor.copy.subtitle)}</div>
        </div>
      </div>
      <p className="ml-hint">{t(descriptor.copy.billingNote)}</p>

      {"account" in state && <AccountLine account={state.account} />}

      <Body
        state={state}
        busy={busy}
        copy={descriptor.copy}
        cliInstallCmd={descriptor.cliInstallCmd}
        apiBilled={account?.auth_mode != null && !account.logged_in}
        onLogin={login}
        onRetry={refresh}
        onInstall={() => setConfirm("install")}
      />

      {state.kind === "ready" && (
        <ModelSection
          state={state}
          copy={descriptor.copy}
          busy={busy}
          picks={picks}
          aliases={aliases}
          registrySet={registrySet}
          manualId={manualId}
          onToggle={(id) => setPicks(togglePick(picks, id))}
          onAlias={(id, alias) => setAliases({ ...aliases, [id]: alias })}
          onManualId={setManualId}
          onRetryModels={refresh}
          onAdd={() =>
            addPicks(
              [...picks].map((id) => ({
                model: id,
                alias: aliases[id]?.trim() || defaultChatGPTAlias(id),
                verified: true,
              })),
            )
          }
          onAddManual={() =>
            addPicks([
              {
                model: manualId.trim(),
                alias: defaultChatGPTAlias(manualId),
                verified: false,
              },
            ])
          }
        />
      )}

      {inRegistry.length > 0 && (
        <section className="ml-reg-list">
          <div className="ml-disc-h">
            <div className="ml-disc-h__title">{t(descriptor.copy.registryTitle)}</div>
          </div>
          <ul className="ml-mlist" role="list">
            {inRegistry.map((m) => (
              <li key={m.ref_name} className="ml-mrow ml-reg-row">
                <span className="ml-mbody">
                  <span className="ml-mbody__id">{m.model}</span>
                  <span className="ml-mbody__alias">
                    <code className="ml-code">{m.ref_name}</code>
                  </span>
                </span>
                <span className="ml-badges">
                  <span className="ml-badge ml-badge--sub">{t("lib.chatgpt.subscriptionBadge")}</span>
                  {m.catalog_verified === false && (
                    <span className="ml-badge ml-badge--unverified">
                      {t("lib.chatgpt.unverifiedBadge")}
                    </span>
                  )}
                </span>
                <span className="ml-reg-actions">
                  <button
                    className="ml-btn ml-btn--sm ml-btn--danger"
                    disabled={busy}
                    onClick={() => removeEntry(m.ref_name)}
                  >
                    {t("lib.deleteBtn")}
                  </button>
                </span>
              </li>
            ))}
          </ul>
          <p className="ml-hint">{t(descriptor.copy.disconnectHint)}</p>
          <button className="ml-btn ml-btn--ghost" disabled={busy} onClick={disconnect}>
            {t(descriptor.copy.disconnectBtn)}
          </button>
        </section>
      )}

      {loggedIn && state.kind !== "login-in-progress" && (
        <section className="ml-chatgpt__logout">
          <button
            className="ml-btn ml-btn--sm ml-btn--danger"
            disabled={busy}
            onClick={() => setConfirm("logout")}
          >
            {t(descriptor.copy.logoutBtn)}
          </button>
        </section>
      )}

      {confirm === "install" && (
        <ConfirmCard
          title={t("lib.chatgpt.gateway.consentTitle")}
          body={t("lib.chatgpt.gateway.consentBody")}
          ok={t("lib.chatgpt.gateway.consentOk")}
          cancel={t("lib.cancelBtn")}
          busy={busy}
          onOk={installGateway}
          onCancel={() => setConfirm(null)}
        />
      )}
      {confirm === "logout" && (
        <ConfirmCard
          title={t(descriptor.copy.logoutConfirmTitle)}
          body={t(descriptor.copy.logoutConfirmBody)}
          ok={t(descriptor.copy.logoutConfirmOk)}
          cancel={t("lib.cancelBtn")}
          busy={busy}
          danger
          onOk={logout}
          onCancel={() => setConfirm(null)}
        />
      )}

      <div aria-live="polite">
        {actionError && <p className="ml-error">{actionError}</p>}
        {notice && <p className="ml-test-ok">{notice}</p>}
      </div>
    </div>
  );
}

function AccountLine({ account }: { account: ChatGPTAccount }) {
  const { t } = useT();
  return (
    <p className="ml-chatgpt__account">
      <span className="ml-badge ml-badge--sub">{t("lib.chatgpt.subscriptionBadge")}</span>{" "}
      {account.email ?? "ChatGPT"}
      {account.plan_type ? ` · ${account.plan_type}` : ""}
    </p>
  );
}

function Body({
  state,
  busy,
  copy,
  cliInstallCmd,
  apiBilled,
  onLogin,
  onRetry,
  onInstall,
}: {
  state: ChatGPTPanelState;
  busy: boolean;
  copy: SubscriptionCopy;
  cliInstallCmd: string;
  /** Signed in to the CLI, but with a credential this provider cannot use. */
  apiBilled: boolean;
  onLogin: () => void;
  onRetry: () => void;
  onInstall: () => void;
}) {
  const { t } = useT();
  switch (state.kind) {
    case "loading":
      return <p className="ml-empty">{t("detail.loading")}</p>;
    case "codex-missing":
      return (
        <div>
          <p className="ml-error" role="alert">
            {t(copy.cliMissing)}
          </p>
          <p className="ml-hint">
            {t(copy.cliInstallHint)} <code className="ml-code">{cliInstallCmd}</code>
          </p>
          <button className="ml-btn ml-btn--ghost" onClick={onRetry}>
            {t("lib.chatgpt.retryBtn")}
          </button>
        </div>
      );
    case "logged-out":
      return (
        <div>
          <p className="ml-hint">{t(apiBilled ? copy.loggedOutApiBilled : copy.loggedOut)}</p>
          <button className="ml-btn ml-btn--primary" disabled={busy} onClick={onLogin}>
            {t(copy.loginBtn)}
          </button>
        </div>
      );
    case "login-in-progress":
      return (
        <p className="ml-hint" aria-live="polite">
          {t(copy.loginInProgress)}
        </p>
      );
    case "account-unavailable":
      return (
        <div>
          <p className="ml-error" role="alert">
            {t(copy.accountUnavailable, { error: state.message })}
          </p>
          <button className="ml-btn ml-btn--ghost" onClick={onRetry}>
            {t("lib.chatgpt.retryBtn")}
          </button>
        </div>
      );
    case "gateway-missing":
      return (
        <div>
          <div className="ml-disc-h__title">{t("lib.chatgpt.gateway.title")}</div>
          <p className="ml-hint">{t("lib.chatgpt.gateway.missing")}</p>
          <button className="ml-btn ml-btn--primary" disabled={busy} onClick={onInstall}>
            {t("lib.chatgpt.gateway.installBtn")}
          </button>
        </div>
      );
    case "gateway-stopped":
      return (
        <div>
          <div className="ml-disc-h__title">{t("lib.chatgpt.gateway.title")}</div>
          <p className="ml-error" role="alert">
            {t(PROBLEM_KEY[state.problem])}
          </p>
          <button className="ml-btn ml-btn--primary" disabled={busy} onClick={onInstall}>
            {t("lib.chatgpt.gateway.repairBtn")}
          </button>
        </div>
      );
    case "models-loading":
      return <p className="ml-hint">{t("lib.chatgpt.modelsLoading")}</p>;
    case "ready":
      return null;
  }
}

function ModelSection({
  state,
  copy,
  busy,
  picks,
  aliases,
  registrySet,
  manualId,
  onToggle,
  onAlias,
  onManualId,
  onRetryModels,
  onAdd,
  onAddManual,
}: {
  state: Extract<ChatGPTPanelState, { kind: "ready" }>;
  copy: SubscriptionCopy;
  busy: boolean;
  picks: Set<string>;
  aliases: Record<string, string>;
  registrySet: Set<string>;
  manualId: string;
  onToggle: (id: string) => void;
  onAlias: (id: string, alias: string) => void;
  onManualId: (v: string) => void;
  onRetryModels: () => void;
  onAdd: () => void;
  onAddManual: () => void;
}) {
  const { t } = useT();
  if (state.modelsError !== null) {
    return (
      <div className="ml-discover">
        <p className="ml-error" role="alert">
          {t("lib.chatgpt.modelsFailed", { error: state.modelsError })}
        </p>
        <button className="ml-btn ml-btn--ghost" disabled={busy} onClick={onRetryModels}>
          {t("lib.chatgpt.retryBtn")}
        </button>
        <div className="ml-field">
          <label className="ml-label" htmlFor="chatgpt-manual-id">
            {t("lib.chatgpt.advancedTitle")}
          </label>
          <input
            id="chatgpt-manual-id"
            className="ml-input"
            placeholder={t("lib.chatgpt.advancedPlaceholder")}
            value={manualId}
            onChange={(e) => onManualId(e.target.value)}
          />
          <p className="ml-hint">{t("lib.chatgpt.advancedHint")}</p>
          <button
            className="ml-btn ml-btn--primary"
            disabled={busy || manualId.trim() === ""}
            onClick={onAddManual}
          >
            {t("lib.chatgpt.advancedAddBtn")}
          </button>
        </div>
      </div>
    );
  }
  return (
    <div className="ml-discover">
      <div className="ml-disc-h">
        <div className="ml-disc-h__title">
          {t(copy.modelsTitle)}
          <span className="ml-disc-h__hint"> · {t(copy.modelsHint)}</span>
        </div>
      </div>
      {state.models.length === 0 ? (
        <p className="ml-hint">{t("lib.noModelsHint")}</p>
      ) : (
        <ul className="ml-mlist" role="group" aria-label={t(copy.modelsTitle)}>
          {state.models.map((m) => {
            const sel = picks.has(m.id);
            const alias = aliases[m.id] ?? defaultChatGPTAlias(m.id);
            return (
              <li key={m.id} className={`ml-mrow${sel ? " ml-mrow--sel" : ""}`}>
                <label className="ml-mrow__pick">
                  <input
                    type="checkbox"
                    className="ml-cbx"
                    checked={sel}
                    onChange={() => onToggle(m.id)}
                    aria-label={m.display_name}
                  />
                  <span className="ml-mbody">
                    <span className="ml-mbody__id">
                      {m.display_name}
                      {m.is_default && (
                        <span className="ml-badge ml-badge--reg">{t("lib.chatgpt.defaultBadge")}</span>
                      )}
                      {registrySet.has(m.id) && (
                        <span className="ml-badge ml-badge--reg">{t("lib.alreadyInRegistry")}</span>
                      )}
                    </span>
                    <span className="ml-mbody__meta">
                      <code className="ml-code">{m.id}</code>
                      {m.input_modalities.length > 0 && ` · ${m.input_modalities.join(", ")}`}
                      {m.reasoning_efforts.length > 0 && ` · ${m.reasoning_efforts.join(" / ")}`}
                    </span>
                  </span>
                </label>
                <span className="ml-mbody__alias">
                  <label className="ml-label" htmlFor={`alias-${m.id}`}>
                    {t("lib.aliasLabel")}
                  </label>
                  <input
                    id={`alias-${m.id}`}
                    className="ml-input ml-input--alias"
                    value={alias}
                    onChange={(e) => onAlias(m.id, e.target.value)}
                  />
                </span>
              </li>
            );
          })}
        </ul>
      )}
      <div className="ml-disc-foot">
        <button
          className="ml-btn ml-btn--primary"
          disabled={busy || picks.size === 0}
          onClick={onAdd}
        >
          {picks.size === 0 ? t("lib.addBtnNone") : t("lib.addBtn", { count: String(picks.size) })}
        </button>
      </div>
    </div>
  );
}

function ConfirmCard({
  title,
  body,
  ok,
  cancel,
  busy,
  danger,
  onOk,
  onCancel,
}: {
  title: string;
  body: string;
  ok: string;
  cancel: string;
  busy: boolean;
  danger?: boolean;
  onOk: () => void;
  onCancel: () => void;
}) {
  return (
    <div className="ml-confirm" role="alertdialog" aria-labelledby="ml-confirm-title" aria-describedby="ml-confirm-body">
      <div id="ml-confirm-title" className="ml-disc-h__title">
        {title}
      </div>
      <p id="ml-confirm-body" className="ml-hint">
        {body}
      </p>
      <div className="ml-confirm__actions">
        <button className="ml-btn ml-btn--ghost" disabled={busy} onClick={onCancel} autoFocus>
          {cancel}
        </button>
        <button
          className={`ml-btn ${danger ? "ml-btn--danger" : "ml-btn--primary"}`}
          disabled={busy}
          onClick={onOk}
        >
          {ok}
        </button>
      </div>
    </div>
  );
}
