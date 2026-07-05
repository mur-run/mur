/**
 * ModelLibrary — full-screen modal for managing model providers and registry.
 *
 * Left rail: connected cloud providers (from list_models), detected local
 * providers (from probe_local_providers), and "新增 Provider" presets.
 * Right pane: delegated to ModelLibraryPanels (ConnectedPanel / LocalPanel /
 * NewProviderPanel).
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ModelOption } from "./modelPicker";
import { CLOUD_PRESETS } from "./modelLibraryHelpers";
import { useT } from "../i18n";
import {
  ConnectedPanel,
  LocalPanel,
  NewProviderPanel,
  providerColor,
  providerInitials,
  type DetectedLocalView,
} from "./ModelLibraryPanels";

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

// ── Props ──────────────────────────────────────────────────────────────────

interface Props {
  open: boolean;
  onClose?: () => void;
  /** Render inline (no overlay/backdrop, no close button) for use as a page. */
  embedded?: boolean;
}

// ── Selection key helpers ──────────────────────────────────────────────────

type PanelKind =
  | { kind: "connected"; providerKey: string }
  | { kind: "local"; detectedKey: string }
  | { kind: "new"; presetKey: string };

// ── Component ──────────────────────────────────────────────────────────────

export function ModelLibrary({ open, onClose, embedded = false }: Props) {
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

  const inner = (
    <div className={embedded ? "ml-win ml-win--embedded" : "ml-win"}>
      {!embedded && (
        <div className="ml-bar">
          <button
            className="ml-dot ml-dot--r"
            onClick={onClose}
            aria-label={t("detail.close")}
          />
          <span className="ml-dot ml-dot--y" />
          <span className="ml-dot ml-dot--g" />
          <span className="ml-bar__title">MUR Hub — {t("lib.title")}</span>
          <button className="ml-bar__close" onClick={onClose} aria-label={t("detail.close")}>
            ×
          </button>
        </div>
      )}

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
                registryModels={registryModels}
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
                registryModels={registryModels}
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
  );

  if (embedded) {
    return inner;
  }

  return (
    <div
      className="ml-overlay"
      role="dialog"
      aria-modal="true"
      aria-label={t("lib.title")}
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose?.();
      }}
    >
      {inner}
    </div>
  );
}
