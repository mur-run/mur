/**
 * ModelLibrary — full-screen modal for managing model providers and registry.
 *
 * Left rail: connected cloud providers (from list_models), detected local
 * providers (from probe_local_providers), and "新增 Provider" presets.
 * Right pane: base-URL + API-key + key-storage segmented control + Test →
 * discovery checklist with badges → Add N.
 */

import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ModelOption } from "../types";
import { formatCost } from "./modelPicker";
import { CLOUD_PRESETS, togglePick, type CloudPreset } from "./modelLibraryHelpers";
import { useT } from "../i18n";

// ── Tauri command return types ─────────────────────────────────────────────

interface EnrichedModelView {
  model: string;
  alias: string;
  input_cost?: number | null;
  output_cost?: number | null;
  context_window?: number | null;
}

interface DetectedLocalView {
  key: string;
  name: string;
  base_url: string;
  models: EnrichedModelView[];
}

type SecretKind = "keychain" | "env" | "file";

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
};

function providerColor(provider: string): string {
  return PROVIDER_COLORS[provider.toLowerCase()] ?? "#6B7688";
}

function providerInitials(provider: string): string {
  return provider.slice(0, 2).toUpperCase();
}

// ── Connected cloud providers derived from list_models ─────────────────────

interface ConnectedProvider {
  key: string;
  name: string;
  color: string;
  initials: string;
  modelCount: number;
}

function deriveConnected(models: ModelOption[]): ConnectedProvider[] {
  const map = new Map<string, number>();
  for (const m of models) {
    map.set(m.provider, (map.get(m.provider) ?? 0) + 1);
  }
  return Array.from(map.entries()).map(([key, count]) => ({
    key,
    name: key.charAt(0).toUpperCase() + key.slice(1),
    color: providerColor(key),
    initials: providerInitials(key),
    modelCount: count,
  }));
}

// ── Alias generator (mirrors the mockup logic) ─────────────────────────────

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

// ── Props ──────────────────────────────────────────────────────────────────

interface Props {
  open: boolean;
  onClose: () => void;
}

// ── Selection key helpers ──────────────────────────────────────────────────

type PanelKind =
  | { kind: "connected"; providerKey: string }
  | { kind: "local"; detectedKey: string }
  | { kind: "new"; presetKey: string };

// ── Component ──────────────────────────────────────────────────────────────

export function ModelLibrary({ open, onClose }: Props) {
  const { t } = useT();

  // ── Registry models (for connected list + "already in registry" badge) ──
  const [registryModels, setRegistryModels] = useState<ModelOption[]>([]);

  // ── Detected local providers ────────────────────────────────────────────
  const [localProviders, setLocalProviders] = useState<DetectedLocalView[]>([]);

  // ── Active panel selection ──────────────────────────────────────────────
  const [panel, setPanel] = useState<PanelKind | null>(null);

  // Load on open
  useEffect(() => {
    if (!open) return;
    invoke<ModelOption[]>("list_models")
      .then(setRegistryModels)
      .catch(() => {});
    invoke<DetectedLocalView[]>("probe_local_providers")
      .then(setLocalProviders)
      .catch(() => {});
  }, [open]);

  // Set default panel once data arrives
  useEffect(() => {
    if (!open || panel !== null) return;
    const connected = deriveConnected(registryModels);
    if (connected.length > 0) {
      setPanel({ kind: "connected", providerKey: connected[0].key });
    } else if (localProviders.length > 0) {
      setPanel({ kind: "local", detectedKey: localProviders[0].key });
    } else if (CLOUD_PRESETS.length > 0) {
      setPanel({ kind: "new", presetKey: CLOUD_PRESETS[0].key });
    }
  }, [open, registryModels, localProviders, panel]);

  // Reset on close
  useEffect(() => {
    if (!open) setPanel(null);
  }, [open]);

  if (!open) return null;

  const connected = deriveConnected(registryModels);
  const registrySet = new Set(registryModels.map((m) => m.model));

  return (
    <div
      className="ml-overlay"
      role="dialog"
      aria-modal="true"
      aria-label={t("lib.title")}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="ml-win">
        {/* Window chrome bar */}
        <div className="ml-bar">
          <span className="ml-dot ml-dot--r" />
          <span className="ml-dot ml-dot--y" />
          <span className="ml-dot ml-dot--g" />
          <span className="ml-bar__title">MUR Hub — {t("lib.title")}</span>
          <button className="ml-bar__close" onClick={onClose} aria-label={t("detail.close")}>
            ×
          </button>
        </div>

        {/* Two-pane layout */}
        <div className="ml-pane">
          {/* Left rail */}
          <nav className="ml-rail" aria-label="Provider navigation">
            {connected.length > 0 && (
              <>
                <div className="ml-rail__h">{t("lib.section.connected")}</div>
                {connected.map((cp) => {
                  const active =
                    panel?.kind === "connected" && panel.providerKey === cp.key;
                  return (
                    <button
                      key={cp.key}
                      className={`ml-prov${active ? " ml-prov--active" : ""}`}
                      onClick={() => setPanel({ kind: "connected", providerKey: cp.key })}
                      title={cp.name}
                    >
                      <span
                        className="ml-logo"
                        style={{ background: cp.color }}
                        aria-hidden="true"
                      >
                        {cp.initials}
                      </span>
                      <span className="ml-prov__name">{cp.name}</span>
                      <span className="ml-pill ml-pill--count">{cp.modelCount}</span>
                    </button>
                  );
                })}
              </>
            )}

            {localProviders.length > 0 && (
              <>
                <div className="ml-rail__h">{t("lib.section.local")}</div>
                {localProviders.map((lp) => {
                  const active =
                    panel?.kind === "local" && panel.detectedKey === lp.key;
                  const port = lp.base_url.match(/:(\d+)/)?.[1] ?? "";
                  return (
                    <button
                      key={lp.key}
                      className={`ml-prov${active ? " ml-prov--active" : ""}`}
                      onClick={() => setPanel({ kind: "local", detectedKey: lp.key })}
                      title={lp.name}
                    >
                      <span
                        className="ml-logo"
                        style={{ background: providerColor(lp.key) }}
                        aria-hidden="true"
                      >
                        {providerInitials(lp.name)}
                      </span>
                      <span className="ml-prov__name">{lp.name}</span>
                      {port && (
                        <span className="ml-pill ml-pill--local">:{port}</span>
                      )}
                    </button>
                  );
                })}
              </>
            )}

            <div className="ml-rail__h">{t("lib.section.add")}</div>
            {CLOUD_PRESETS.map((preset) => {
              const active =
                panel?.kind === "new" && panel.presetKey === preset.key;
              return (
                <button
                  key={preset.key}
                  className={`ml-prov ml-prov--add${active ? " ml-prov--active" : ""}`}
                  onClick={() => setPanel({ kind: "new", presetKey: preset.key })}
                  title={preset.name}
                >
                  <span
                    className="ml-logo"
                    style={{ background: preset.color }}
                    aria-hidden="true"
                  >
                    {preset.logo}
                  </span>
                  <span className="ml-prov__name ml-prov__name--link">{preset.name}</span>
                </button>
              );
            })}
          </nav>

          {/* Right work pane */}
          <div className="ml-work">
            {panel === null ? (
              <p className="ml-empty">{t("detail.loading")}</p>
            ) : panel.kind === "connected" ? (
              <ConnectedPanel
                providerKey={panel.providerKey}
                registryModels={registryModels}
                registrySet={registrySet}
                onModelsAdded={() =>
                  invoke<ModelOption[]>("list_models")
                    .then(setRegistryModels)
                    .catch(() => {})
                }
              />
            ) : panel.kind === "local" ? (
              <LocalPanel
                detected={localProviders.find((lp) => lp.key === panel.detectedKey)!}
                registrySet={registrySet}
                onModelsAdded={() =>
                  invoke<ModelOption[]>("list_models")
                    .then(setRegistryModels)
                    .catch(() => {})
                }
              />
            ) : (
              <NewProviderPanel
                preset={CLOUD_PRESETS.find((p) => p.key === panel.presetKey)!}
                registrySet={registrySet}
                onModelsAdded={() =>
                  invoke<ModelOption[]>("list_models")
                    .then(setRegistryModels)
                    .catch(() => {})
                }
              />
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

// ── ConnectedPanel — re-explore an already-connected cloud provider ─────────

function ConnectedPanel({
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

  // Derive base URL and count from registry
  const provModels = registryModels.filter((m) => m.provider === providerKey);
  const existingBaseUrl = "";

  const [baseUrl, setBaseUrl] = useState(existingBaseUrl);
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
      // Pre-select already-in-registry models
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

      <div className="ml-field">
        <label className="ml-label">{t("lib.baseUrl")}</label>
        <input
          className="ml-input"
          type="url"
          value={baseUrl}
          placeholder="https://…"
          onChange={(e) => setBaseUrl(e.target.value)}
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

function LocalPanel({
  detected,
  registrySet,
  onModelsAdded,
}: {
  detected: DetectedLocalView;
  registrySet: Set<string>;
  onModelsAdded: () => void;
}) {
  const { t } = useT();
  const color = providerColor(detected.key);
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

function NewProviderPanel({
  preset,
  registrySet,
  onModelsAdded,
}: {
  preset: CloudPreset;
  registrySet: Set<string>;
  onModelsAdded: () => void;
}) {
  const { t } = useT();
  const [baseUrl, setBaseUrl] = useState(preset.baseUrl);
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
    setBaseUrl(preset.baseUrl);
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

      <div className="ml-field">
        <label className="ml-label">{t("lib.baseUrl")}</label>
        <input
          className="ml-input"
          type="url"
          value={baseUrl}
          onChange={(e) => setBaseUrl(e.target.value)}
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

// ── ModelChecklist — shared discovery results with checkboxes ───────────────

function ModelChecklist({
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
