/**
 * ModelLibraryPanels — right-pane panels for ModelLibrary.
 *
 * Exports: ConnectedPanel, LocalPanel, NewProviderPanel, ModelChecklist
 * All shared types (EnrichedModelView, DetectedLocalView, SecretKind) also
 * exported so ModelLibrary.tsx can import them.
 */

import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ModelOption } from "./modelPicker";
import { formatCost } from "./modelPicker";
import { togglePick, type CloudPreset } from "./modelLibraryHelpers";
import { useT } from "../i18n";

/**
 * Endpoint the backend suggests for `provider` before the user edits the
 * field: an existing registry entry, else a live local OAuth bridge
 * (mur-model-gateway). `null` until it answers, and for a provider with
 * neither — callers fall back to the vendor default.
 *
 * A provider behind a gateway must not be discovered against the vendor host:
 * its stored token is a gateway token and the vendor answers 401. Asking the
 * backend keeps that precedence in one place (`suggest_base_url`).
 */
function useSuggestedBaseUrl(provider: string): string | null {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    setUrl(null);
    invoke<string | null>("suggested_base_url", { provider })
      .then((u) => {
        if (live) setUrl(u ?? null);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [provider]);
  return url;
}

// ── Tauri command return types ─────────────────────────────────────────────

export interface EnrichedModelView {
  model: string;
  alias: string;
  input_cost?: number | null;
  output_cost?: number | null;
  context_window?: number | null;
}

export interface DetectedLocalView {
  key: string;
  name: string;
  base_url: string;
  models: EnrichedModelView[];
}

export type SecretKind = "keychain" | "env" | "file";

// ── Provider avatar helpers ────────────────────────────────────────────────

const PROVIDER_COLORS: Record<string, string> = {
  anthropic: "#C5694A",
  deepseek: "#4A6CF0",
  ollama: "#6B7688",
  openai: "#10A37F",
  google: "#E8543F",
  cohere: "#3B82F6",
  mistral: "#FF7000",
  local: "#6B7688",
  openrouter: "#8B5CF6",
  xai: "#111111",
  groq: "#F55036",
  together: "#0F6FFF",
  fireworks: "#9215FF",
};

export function providerColor(provider: string): string {
  return PROVIDER_COLORS[provider.toLowerCase()] ?? "#6B7688";
}

export function providerInitials(provider: string): string {
  return provider.slice(0, 2).toUpperCase();
}

// ── Alias generator ────────────────────────────────────────────────────────

function aliasFor(provider: string, modelId: string): string {
  const slug = modelId
    .replace(/[/:.]/g, "_")
    .replace(/-/g, "_")
    .toLowerCase();
  return `${provider}_${slug}`.slice(0, 40);
}

// ── Context-window formatter ───────────────────────────────────────────────

function fmtCtx(ctx?: number | null): string | null {
  if (ctx == null) return null;
  return ctx >= 1000 ? `${Math.round(ctx / 1000)}k ctx` : `${ctx} ctx`;
}

// ── ModelChecklist — shared discovery results with checkboxes ───────────────

export function ModelChecklist({
  models,
  providerKey,
  registrySet,
  picks,
  onToggle,
  onAdd,
  addError,
  addedOk,
  isLocal,
}: {
  models: EnrichedModelView[];
  providerKey: string;
  registrySet: Set<string>;
  picks: Set<string>;
  onToggle: (id: string) => void;
  onAdd: () => void;
  addError: string | null;
  addedOk: number | null;
  isLocal: boolean;
}) {
  const { t } = useT();

  return (
    <div className="ml-discover">
      <div className="ml-disc-h">
        <div className="ml-disc-h__title">
          {t("lib.modelsSection")}
          <span className="ml-disc-h__hint"> · {t("lib.modelsSectionHint")}</span>
        </div>
      </div>

      {models.length === 0 ? (
        <p className="ml-hint">{t("lib.noModelsHint")}</p>
      ) : (
        <ul className="ml-mlist" role="group" aria-label={t("lib.modelsSection")}>
          {models.map((m) => {
            const sel = picks.has(m.model);
            const inReg = registrySet.has(m.model);
            const inCost = formatCost(m.input_cost);
            const outCost = formatCost(m.output_cost);
            const ctx = fmtCtx(m.context_window);
            const alias = aliasFor(providerKey, m.model);

            return (
              <li
                key={m.model}
                className={`ml-mrow${sel ? " ml-mrow--sel" : ""}`}
                onClick={() => onToggle(m.model)}
                role="checkbox"
                aria-checked={sel}
                tabIndex={0}
                onKeyDown={(e) => {
                  if (e.key === " " || e.key === "Enter") {
                    e.preventDefault();
                    onToggle(m.model);
                  }
                }}
              >
                <span className="ml-cbx" aria-hidden="true">
                  {sel && (
                    <svg
                      width="11"
                      height="11"
                      fill="none"
                      stroke="#fff"
                      strokeWidth="2.4"
                    >
                      <path d="M2 5.5L4.5 8L9 3" />
                    </svg>
                  )}
                </span>
                <span className="ml-mbody">
                  <span className="ml-mbody__id">
                    {m.model}
                    {inReg && (
                      <span className="ml-badge ml-badge--reg">
                        {t("lib.alreadyInRegistry")}
                      </span>
                    )}
                  </span>
                  <span className="ml-mbody__alias">
                    {t("lib.aliasLabel")}{" "}
                    <code className="ml-code">{m.alias || alias}</code>
                  </span>
                </span>
                <span className="ml-badges">
                  {isLocal ? (
                    <span className="ml-badge ml-badge--local">local</span>
                  ) : (
                    <span className="ml-badge ml-badge--frontier">frontier</span>
                  )}
                  {inCost !== null && (
                    <span className="ml-badge ml-badge--in">in {inCost}</span>
                  )}
                  {outCost !== null && (
                    <span className="ml-badge ml-badge--out">out {outCost}</span>
                  )}
                  {ctx !== null && (
                    <span className="ml-badge ml-badge--ctx">{ctx}</span>
                  )}
                </span>
              </li>
            );
          })}
        </ul>
      )}

      <div className="ml-disc-foot">
        <span className="ml-disc-foot__note">
          {t("lib.aliasLabel")} → model_ref
        </span>
        <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
          {addedOk !== null && (
            <span className="ml-test-ok">
              {t("lib.addedOk", { count: addedOk })}
            </span>
          )}
          <button
            className="ml-btn ml-btn--primary"
            onClick={onAdd}
            disabled={picks.size === 0}
          >
            {picks.size === 0
              ? t("lib.addBtnNone")
              : t("lib.addBtn", { count: picks.size })}
          </button>
        </div>
      </div>

      {addError && <p className="ml-error">{addError}</p>}
    </div>
  );
}

// ── RegistryList — models already in ~/.mur/models.yaml: view / edit cost / delete ──

export function RegistryList({
  models,
  onChanged,
}: {
  models: ModelOption[];
  onChanged: () => void;
}) {
  const { t } = useT();
  const [editing, setEditing] = useState<string | null>(null);
  const [inDraft, setInDraft] = useState("");
  const [outDraft, setOutDraft] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  if (models.length === 0) return null;

  function startEdit(m: ModelOption) {
    setEditing(m.ref_name);
    setInDraft(m.input_cost != null ? String(m.input_cost) : "");
    setOutDraft(m.output_cost != null ? String(m.output_cost) : "");
    setError(null);
  }

  async function saveEdit(refName: string) {
    setBusy(refName);
    setError(null);
    try {
      await invoke("update_model", {
        refName,
        inputCost: inDraft.trim() === "" ? null : Number(inDraft),
        outputCost: outDraft.trim() === "" ? null : Number(outDraft),
      });
      setEditing(null);
      onChanged();
    } catch (e) {
      setError(t("lib.editError", { error: String(e) }));
    } finally {
      setBusy(null);
    }
  }

  async function handleRemove(refName: string) {
    setBusy(refName);
    setError(null);
    try {
      await invoke("remove_model", { refName });
      onChanged();
    } catch (e) {
      setError(t("lib.removeError", { error: String(e) }));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="ml-reg-list">
      <div className="ml-disc-h">
        <div className="ml-disc-h__title">{t("lib.registrySection")}</div>
      </div>
      <ul className="ml-mlist" role="list">
        {models.map((m) => {
          const inCost = formatCost(m.input_cost);
          const outCost = formatCost(m.output_cost);
          const ctx = fmtCtx(m.context_window);
          const isBusy = busy === m.ref_name;
          return (
            <li key={m.ref_name}>
              <div className="ml-mrow ml-reg-row">
                <span className="ml-mbody">
                  <span className="ml-mbody__id">{m.model}</span>
                  <span className="ml-mbody__alias">
                    <code className="ml-code">{m.ref_name}</code>
                  </span>
                </span>
                <span className="ml-badges">
                  {inCost !== null && (
                    <span className="ml-badge ml-badge--in">in {inCost}</span>
                  )}
                  {outCost !== null && (
                    <span className="ml-badge ml-badge--out">out {outCost}</span>
                  )}
                  {ctx !== null && <span className="ml-badge ml-badge--ctx">{ctx}</span>}
                </span>
                <span className="ml-reg-actions">
                  <button
                    className="ml-btn ml-btn--sm ml-btn--ghost"
                    onClick={() => startEdit(m)}
                    disabled={isBusy}
                  >
                    {t("lib.editBtn")}
                  </button>
                  <button
                    className="ml-btn ml-btn--sm ml-btn--danger"
                    onClick={() => handleRemove(m.ref_name)}
                    disabled={isBusy}
                  >
                    {t("lib.deleteBtn")}
                  </button>
                </span>
              </div>
              {editing === m.ref_name && (
                <div className="ml-edit-row">
                  <div className="ml-field">
                    <label className="ml-label">{t("lib.inputCost")}</label>
                    <input
                      className="ml-input"
                      type="number"
                      step="0.0001"
                      min="0"
                      value={inDraft}
                      onChange={(e) => setInDraft(e.target.value)}
                    />
                  </div>
                  <div className="ml-field">
                    <label className="ml-label">{t("lib.outputCost")}</label>
                    <input
                      className="ml-input"
                      type="number"
                      step="0.0001"
                      min="0"
                      value={outDraft}
                      onChange={(e) => setOutDraft(e.target.value)}
                    />
                  </div>
                  <button
                    className="ml-btn ml-btn--sm ml-btn--primary"
                    onClick={() => saveEdit(m.ref_name)}
                    disabled={isBusy}
                  >
                    {t("lib.saveBtn")}
                  </button>
                  <button
                    className="ml-btn ml-btn--sm ml-btn--ghost"
                    onClick={() => setEditing(null)}
                  >
                    {t("lib.cancelBtn")}
                  </button>
                </div>
              )}
            </li>
          );
        })}
      </ul>
      {error && <p className="ml-error">{error}</p>}
    </div>
  );
}

// ── ConnectedPanel — re-explore an already-connected cloud provider ─────────

export function ConnectedPanel({
  providerKey,
  registryModels,
  registrySet,
  onModelsAdded,
}: {
  providerKey: string;
  registryModels: ModelOption[];
  registrySet: Set<string>;
  onModelsAdded: () => void;
}) {
  const { t } = useT();
  const providerName =
    providerKey.charAt(0).toUpperCase() + providerKey.slice(1);
  const color = providerColor(providerKey);

  const provModels = registryModels.filter(
    (m) => (m.vendor || m.provider) === providerKey,
  );
  const suggested = useSuggestedBaseUrl(providerKey);

  const [baseUrlEdit, setBaseUrlEdit] = useState<string | null>(null);
  // The suggestion arrives async — read through to it until the user types.
  const baseUrl = baseUrlEdit ?? suggested ?? "";
  const [discovered, setDiscovered] = useState<EnrichedModelView[]>([]);
  const [picks, setPicks] = useState<Set<string>>(new Set());
  const [testing, setTesting] = useState(false);
  const [showDiscover, setShowDiscover] = useState(false);
  const [testError, setTestError] = useState<string | null>(null);
  const [addError, setAddError] = useState<string | null>(null);
  const [addedOk, setAddedOk] = useState<number | null>(null);

  async function handleTest() {
    setTesting(true);
    setTestError(null);
    try {
      const result = await invoke<EnrichedModelView[]>("test_provider", {
        provider: providerKey,
        baseUrl: baseUrl.trim() || undefined,
        apiKey: undefined,
      });
      setDiscovered(result);
      const pre = new Set(
        result.filter((m) => registrySet.has(m.model)).map((m) => m.model)
      );
      setPicks(pre);
      setShowDiscover(true);
    } catch (e) {
      setTestError(t("lib.testError", { error: String(e) }));
    } finally {
      setTesting(false);
    }
  }

  async function handleAdd() {
    const pickList = discovered
      .filter((m) => picks.has(m.model))
      .map((m) => ({ model: m.model, alias: m.alias }));
    if (pickList.length === 0) return;
    setAddError(null);
    try {
      await invoke("add_models", {
        provider: providerKey,
        baseUrl: baseUrl.trim() || undefined,
        tier: "frontier",
        secretKind: "keychain",
        secretValue: undefined,
        picks: pickList,
      });
      setAddedOk(pickList.length);
      onModelsAdded();
      setTimeout(() => setAddedOk(null), 4000);
    } catch (e) {
      setAddError(t("lib.addError", { error: String(e) }));
    }
  }

  return (
    <div>
      <div className="ml-crumb">
        {t("lib.title")} <b>›</b> {t("lib.crumbCloud")} <b>›</b> {providerName}
      </div>
      <div className="ml-panel-h">
        <span className="ml-logo ml-logo--lg" style={{ background: color }}>
          {providerInitials(providerKey)}
        </span>
        <div>
          <h2 className="ml-panel-h__title">{providerName}</h2>
          <div className="ml-panel-h__sub">
            {provModels.length > 0
              ? t("lib.cloud.subtitle")
              : t("lib.cloud.subtitleNew")}
          </div>
        </div>
      </div>

      <RegistryList models={provModels} onChanged={onModelsAdded} />

      <div className="ml-field">
        <label className="ml-label">{t("lib.baseUrl")}</label>
        <input
          className="ml-input"
          type="url"
          value={baseUrl}
          placeholder="https://…"
          onChange={(e) => setBaseUrlEdit(e.target.value)}
        />
        <div className="ml-hint">{t("lib.baseUrlHint")}</div>
      </div>

      <div className="ml-row-actions">
        <button
          className="ml-btn ml-btn--primary"
          onClick={handleTest}
          disabled={testing}
        >
          {testing ? (
            <>
              <span className="ml-spinner" aria-hidden="true" />
              {t("lib.testing")}
            </>
          ) : (
            t("lib.retestBtn")
          )}
        </button>
        {showDiscover && !testError && (
          <span className="ml-test-ok">
            ✓ {t("lib.connected")} · {discovered.length}
          </span>
        )}
      </div>

      {testError && <p className="ml-error">{testError}</p>}

      {showDiscover && (
        <ModelChecklist
          models={discovered}
          providerKey={providerKey}
          registrySet={registrySet}
          picks={picks}
          onToggle={(id) => setPicks(togglePick(picks, id))}
          onAdd={handleAdd}
          addError={addError}
          addedOk={addedOk}
          isLocal={false}
        />
      )}
    </div>
  );
}

// ── LocalPanel — auto-detected local provider (no key needed) ───────────────

export function LocalPanel({
  detected,
  registryModels,
  registrySet,
  onModelsAdded,
}: {
  detected: DetectedLocalView;
  registryModels: ModelOption[];
  registrySet: Set<string>;
  onModelsAdded: () => void;
}) {
  const { t } = useT();
  const color = providerColor(detected.key);
  const provModels = registryModels.filter(
    (m) => (m.vendor || m.provider) === detected.key,
  );
  const [picks, setPicks] = useState<Set<string>>(
    new Set(
      detected.models.filter((m) => registrySet.has(m.model)).map((m) => m.model)
    )
  );
  const [addError, setAddError] = useState<string | null>(null);
  const [addedOk, setAddedOk] = useState<number | null>(null);

  async function handleAdd() {
    const pickList = detected.models
      .filter((m) => picks.has(m.model))
      .map((m) => ({ model: m.model, alias: m.alias }));
    if (pickList.length === 0) return;
    setAddError(null);
    try {
      await invoke("add_models", {
        provider: detected.key,
        baseUrl: detected.base_url,
        tier: "local",
        secretKind: "keychain",
        secretValue: undefined,
        picks: pickList,
      });
      setAddedOk(pickList.length);
      onModelsAdded();
      setTimeout(() => setAddedOk(null), 4000);
    } catch (e) {
      setAddError(t("lib.addError", { error: String(e) }));
    }
  }

  return (
    <div>
      <div className="ml-crumb">
        {t("lib.title")} <b>›</b> {t("lib.crumbLocal")} <b>›</b> {detected.name}
      </div>
      <div className="ml-panel-h">
        <span className="ml-logo ml-logo--lg" style={{ background: color }}>
          {providerInitials(detected.name)}
        </span>
        <div>
          <h2 className="ml-panel-h__title">{detected.name}</h2>
          <div className="ml-panel-h__sub">{t("lib.local.subtitle")}</div>
        </div>
      </div>

      <RegistryList models={provModels} onChanged={onModelsAdded} />

      <div className="ml-field">
        <label className="ml-label">{t("lib.baseUrl")}</label>
        <input
          className="ml-input"
          type="url"
          value={detected.base_url}
          readOnly
        />
      </div>

      <div className="ml-field">
        <label className="ml-label">{t("lib.detectStatus")}</label>
        <div className="ml-test-ok ml-test-ok--block">
          ✓ {t("lib.detectedOk", { count: detected.models.length })}
        </div>
        <div className="ml-hint">
          {t("lib.localHint", { name: detected.name })}
        </div>
      </div>

      <ModelChecklist
        models={detected.models}
        providerKey={detected.key}
        registrySet={registrySet}
        picks={picks}
        onToggle={(id) => setPicks(togglePick(picks, id))}
        onAdd={handleAdd}
        addError={addError}
        addedOk={addedOk}
        isLocal={true}
      />
    </div>
  );
}

// ── NewProviderPanel — cloud preset not yet connected ──────────────────────

export function NewProviderPanel({
  preset,
  registryModels,
  registrySet,
  onModelsAdded,
}: {
  preset: CloudPreset;
  registryModels: ModelOption[];
  registrySet: Set<string>;
  onModelsAdded: () => void;
}) {
  const provModels = registryModels.filter((m) => m.provider === preset.key);
  const { t } = useT();
  const [baseUrlEdit, setBaseUrlEdit] = useState<string | null>(null);
  // A gateway-backed provider must not be re-tested against the vendor host,
  // so the endpoint already in use outranks the preset default. The suggestion
  // arrives async — read through to it until the user types.
  const suggested = useSuggestedBaseUrl(preset.key);
  const baseUrl = baseUrlEdit ?? suggested ?? preset.baseUrl;
  const [apiKey, setApiKey] = useState("");
  const [secretKind, setSecretKind] = useState<SecretKind>("keychain");
  const [testing, setTesting] = useState(false);
  const [testOk, setTestOk] = useState(false);
  const [testError, setTestError] = useState<string | null>(null);
  const [discovered, setDiscovered] = useState<EnrichedModelView[]>([]);
  const [picks, setPicks] = useState<Set<string>>(new Set());
  const [showDiscover, setShowDiscover] = useState(false);
  const [addError, setAddError] = useState<string | null>(null);
  const [addedOk, setAddedOk] = useState<number | null>(null);

  // Reset when preset changes
  const prevKey = useRef(preset.key);
  if (prevKey.current !== preset.key) {
    prevKey.current = preset.key;
    setBaseUrlEdit(null);
    setApiKey("");
    setSecretKind("keychain");
    setTesting(false);
    setTestOk(false);
    setTestError(null);
    setDiscovered([]);
    setPicks(new Set());
    setShowDiscover(false);
    setAddError(null);
    setAddedOk(null);
  }

  async function handleTest() {
    setTesting(true);
    setTestError(null);
    setTestOk(false);
    try {
      const result = await invoke<EnrichedModelView[]>("test_provider", {
        provider: preset.key,
        baseUrl: baseUrl.trim() || undefined,
        apiKey: apiKey.trim() || undefined,
      });
      setDiscovered(result);
      const pre = new Set(
        result.filter((m) => registrySet.has(m.model)).map((m) => m.model)
      );
      setPicks(pre);
      setTestOk(true);
      setShowDiscover(true);
    } catch (e) {
      setTestError(t("lib.testError", { error: String(e) }));
    } finally {
      setTesting(false);
    }
  }

  async function handleAdd() {
    const pickList = discovered
      .filter((m) => picks.has(m.model))
      .map((m) => ({ model: m.model, alias: m.alias }));
    if (pickList.length === 0) return;
    setAddError(null);
    try {
      await invoke("add_models", {
        provider: preset.key,
        baseUrl: baseUrl.trim() || preset.baseUrl,
        tier: "frontier",
        secretKind,
        secretValue: apiKey.trim() || undefined,
        picks: pickList,
      });
      setAddedOk(pickList.length);
      onModelsAdded();
      setTimeout(() => setAddedOk(null), 4000);
    } catch (e) {
      setAddError(t("lib.addError", { error: String(e) }));
    }
  }

  return (
    <div>
      <div className="ml-crumb">
        {t("lib.title")} <b>›</b> {t("lib.section.add")} <b>›</b> {preset.name}
      </div>
      <div className="ml-panel-h">
        <span className="ml-logo ml-logo--lg" style={{ background: preset.color }}>
          {preset.logo}
        </span>
        <div>
          <h2 className="ml-panel-h__title">{preset.name}</h2>
          <div className="ml-panel-h__sub">{t("lib.cloud.subtitleNew")}</div>
        </div>
      </div>

      <RegistryList models={provModels} onChanged={onModelsAdded} />

      <div className="ml-field">
        <label className="ml-label">{t("lib.baseUrl")}</label>
        <input
          className="ml-input"
          type="url"
          value={baseUrl}
          onChange={(e) => setBaseUrlEdit(e.target.value)}
        />
        <div className="ml-hint">{t("lib.baseUrlHint")}</div>
      </div>

      <div className="ml-field">
        <label className="ml-label">{t("lib.apiKey")}</label>
        <input
          className="ml-input"
          type="password"
          value={apiKey}
          placeholder={t("lib.apiKeyPlaceholder")}
          autoComplete="off"
          onChange={(e) => setApiKey(e.target.value)}
        />
        <div className="ml-hint" style={{ marginBottom: 8 }}>
          {t("lib.keyStorage")}
        </div>
        <div className="ml-seg">
          {(["keychain", "env", "file"] as SecretKind[]).map((kind) => (
            <button
              key={kind}
              type="button"
              className={`ml-seg__btn${secretKind === kind ? " ml-seg__btn--on" : ""}`}
              onClick={() => setSecretKind(kind)}
            >
              {t(`lib.${kind}` as "lib.keychain" | "lib.env" | "lib.file")}
            </button>
          ))}
        </div>
        <div className="ml-hint">{t("lib.keyStorageHint")}</div>
      </div>

      <div className="ml-row-actions">
        <button
          className="ml-btn ml-btn--primary"
          onClick={handleTest}
          disabled={testing}
        >
          {testing ? (
            <>
              <span className="ml-spinner" aria-hidden="true" />
              {t("lib.testing")}
            </>
          ) : (
            t("lib.testBtn")
          )}
        </button>
        {testOk && (
          <span className="ml-test-ok">
            ✓ {t("lib.connected")} · {discovered.length}
          </span>
        )}
      </div>

      {testError && <p className="ml-error">{testError}</p>}

      {showDiscover && (
        <ModelChecklist
          models={discovered}
          providerKey={preset.key}
          registrySet={registrySet}
          picks={picks}
          onToggle={(id) => setPicks(togglePick(picks, id))}
          onAdd={handleAdd}
          addError={addError}
          addedOk={addedOk}
          isLocal={false}
        />
      )}
    </div>
  );
}
