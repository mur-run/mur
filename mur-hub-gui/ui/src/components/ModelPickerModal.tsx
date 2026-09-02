/**
 * ModelPickerModal — pick the brain the concierge runs on.
 *
 * Two callers: the mandatory first-run case (no model configured at all —
 * `dismissible` off, so there is no way out but to choose) and the "connect a
 * smarter brain" nudge (`dismissible` on).
 *
 * Two paths to configure a model:
 * 1. Download MUR's bundled local model (~1.6 GB) — invoke('download_local_model'),
 *    progress tracked via 'model-download-progress' events, closes on 'model-download-done'.
 * 2. Connect a cloud/local provider via the existing ModelLibrary connect flow,
 *    then pick a registry model → invoke('use_registry_model', { refName }) → close.
 */

import { useEffect, useState } from "react";
import { billingKey } from "./chatgptSubscription";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useT } from "../i18n";
import { downloadProgress } from "./modelPickerProgress";
import type { ModelOption } from "./modelPicker";
import { CLOUD_PRESETS } from "./modelLibraryHelpers";
import {
  NewProviderPanel,
  LocalPanel,
  providerColor,
  providerInitials,
  type DetectedLocalView,
} from "./ModelLibraryPanels";

// ── Payload types ──────────────────────────────────────────────────────────

interface DownloadProgressPayload {
  done: number;
  total: number;
}

// ── Section tabs ───────────────────────────────────────────────────────────

type Section = "local-download" | "connect";

// ── Main component ─────────────────────────────────────────────────────────

interface Props {
  isOpen: boolean;
  onClose: () => void;
  /**
   * Let the user walk away (✕ and backdrop click). Off by default: the
   * first-run case is mandatory because nothing works without a model. The
   * "connect a smarter brain" nudge opens the same picker as an *offer*, so it
   * passes true.
   */
  dismissible?: boolean;
}

export function ModelPickerModal({ isOpen, onClose, dismissible = false }: Props) {
  const { t } = useT();

  // Download tile state
  const [downloading, setDownloading] = useState(false);
  const [downloadDone, setDownloadDone] = useState(false);
  const [downloadError, setDownloadError] = useState<string | null>(null);
  const [progressDone, setProgressDone] = useState(0);
  const [progressTotal, setProgressTotal] = useState(0);

  // Connect section state
  const [section, setSection] = useState<Section>("local-download");
  const [registryModels, setRegistryModels] = useState<ModelOption[]>([]);
  const [localProviders, setLocalProviders] = useState<DetectedLocalView[]>([]);
  const [connectKey, setConnectKey] = useState<string>(CLOUD_PRESETS[0]?.key ?? "openai");
  const [pickedRef, setPickedRef] = useState<string>("");
  const [useRefError, setUseRefError] = useState<string | null>(null);
  const [useRefDone, setUseRefDone] = useState(false);

  // Reset when modal opens
  useEffect(() => {
    if (!isOpen) return;
    setDownloading(false);
    setDownloadDone(false);
    setDownloadError(null);
    setProgressDone(0);
    setProgressTotal(0);
    setSection("local-download");
    setConnectKey(CLOUD_PRESETS[0]?.key ?? "openai");
    setPickedRef("");
    setUseRefError(null);
    setUseRefDone(false);
    invoke<ModelOption[]>("list_models").then(setRegistryModels).catch(() => {});
    invoke<DetectedLocalView[]>("probe_local_providers").then(setLocalProviders).catch(() => {});
  }, [isOpen]);

  // Subscribe to download progress and done events
  useEffect(() => {
    if (!isOpen) return;

    const progressP = listen<DownloadProgressPayload>("model-download-progress", (e) => {
      setProgressDone(e.payload.done);
      setProgressTotal(e.payload.total);
    });

    const doneP = listen("model-download-done", () => {
      setDownloadDone(true);
      setDownloading(false);
      onClose();
    });

    return () => {
      progressP.then((fn) => fn());
      doneP.then((fn) => fn());
    };
  }, [isOpen, onClose]);

  if (!isOpen) return null;

  async function startDownload() {
    setDownloading(true);
    setDownloadError(null);
    setProgressDone(0);
    setProgressTotal(0);
    try {
      await invoke("download_local_model");
      // invoke resolves on success — close if event hasn't already done so
      setDownloadDone(true);
      setDownloading(false);
      onClose();
    } catch (e) {
      setDownloading(false);
      setDownloadError(String(e));
    }
  }

  function refreshRegistry() {
    invoke<ModelOption[]>("list_models").then(setRegistryModels).catch(() => {});
  }

  async function handleUseModel() {
    if (!pickedRef.trim()) return;
    setUseRefError(null);
    try {
      await invoke("use_registry_model", { refName: pickedRef.trim() });
      setUseRefDone(true);
      onClose();
    } catch (e) {
      setUseRefError(String(e));
    }
  }

  const prog = downloadProgress(progressDone, progressTotal);
  const isLocal = localProviders.some((lp) => lp.key === connectKey);
  const localProv = isLocal ? localProviders.find((lp) => lp.key === connectKey) : undefined;
  const cloudPreset = CLOUD_PRESETS.find((p) => p.key === connectKey);
  const registrySet = new Set(registryModels.map((m) => m.model));

  // Rail items: local providers first, then cloud presets
  const railItems = [
    ...localProviders.map((lp) => ({
      key: lp.key,
      name: lp.name,
      color: providerColor(lp.key),
      initials: providerInitials(lp.name),
      isLocal: true,
    })),
    ...CLOUD_PRESETS.map((p) => ({
      key: p.key,
      name: p.name,
      color: p.color,
      initials: p.logo,
      isLocal: false,
    })),
  ];

  return (
    // Backdrop closes only when the picker is an offer, not a requirement.
    <div
      className="wz-overlay"
      role="dialog"
      aria-modal="true"
      onMouseDown={(e) => {
        if (dismissible && e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="wz-modal"
        style={{ width: 560, maxWidth: "95vw" }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="wz-header" style={{ padding: "18px 20px 14px" }}>
          <div>
            <h2 style={{ margin: 0, fontSize: 18, fontWeight: 700 }}>
              {t("modelPicker.title")}
            </h2>
            <p style={{ margin: "6px 0 0", fontSize: 13, color: "var(--text-secondary, #888)" }}>
              {t("modelPicker.subtitle")}
            </p>
          </div>
          {dismissible && (
            <button
              className="wz-close"
              onClick={onClose}
              aria-label={t("wizard.close")}
            >
              ✕
            </button>
          )}
        </div>

        {/* Section tabs */}
        <div
          style={{
            display: "flex",
            borderBottom: "1px solid var(--border-line)",
            paddingInline: 20,
            gap: 4,
          }}
        >
          <TabBtn
            active={section === "local-download"}
            onClick={() => setSection("local-download")}
            label={t("modelPicker.tab.local")}
          />
          <TabBtn
            active={section === "connect"}
            onClick={() => setSection("connect")}
            label={t("modelPicker.tab.connect")}
          />
        </div>

        <div style={{ padding: "16px 20px 20px", overflowY: "auto", maxHeight: "60vh" }}>
          {section === "local-download" && (
            <LocalDownloadSection
              downloading={downloading}
              downloadDone={downloadDone}
              downloadError={downloadError}
              prog={prog}
              progressDone={progressDone}
              progressTotal={progressTotal}
              onStart={startDownload}
              onRetry={startDownload}
            />
          )}

          {section === "connect" && (
            <>
              {/* Provider mini-rail */}
              <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginBottom: 16 }}>
                {railItems.map((prov) => (
                  <button
                    key={prov.key}
                    onClick={() => setConnectKey(prov.key)}
                    style={{
                      display: "flex",
                      alignItems: "center",
                      gap: 6,
                      padding: "5px 10px",
                      borderRadius: 6,
                      border: connectKey === prov.key
                        ? "1px solid var(--accent, #3b82f6)"
                        : "1px solid var(--border-line)",
                      background: connectKey === prov.key
                        ? "var(--surface-hover)"
                        : "var(--surface-card)",
                      cursor: "pointer",
                      fontSize: 12,
                      fontWeight: connectKey === prov.key ? 600 : 400,
                    }}
                  >
                    <span
                      style={{
                        width: 20,
                        height: 20,
                        borderRadius: 4,
                        background: prov.color,
                        display: "flex",
                        alignItems: "center",
                        justifyContent: "center",
                        fontSize: 10,
                        fontWeight: 700,
                        color: "#fff",
                        flexShrink: 0,
                      }}
                    >
                      {prov.initials}
                    </span>
                    {prov.name}
                    {prov.isLocal && (
                      <span
                        style={{
                          fontSize: 10,
                          padding: "1px 4px",
                          borderRadius: 3,
                          background: "var(--surface-hover)",
                          color: "var(--text-secondary, #888)",
                        }}
                      >
                        {t("modelPicker.local.badge")}
                      </span>
                    )}
                  </button>
                ))}
              </div>

              {/* Active connect panel */}
              <div style={{ border: "1px solid var(--border-line)", borderRadius: 8, overflow: "hidden" }}>
                {isLocal && localProv ? (
                  <LocalPanel
                    detected={localProv}
                    registryModels={registryModels}
                    registrySet={registrySet}
                    onModelsAdded={refreshRegistry}
                  />
                ) : cloudPreset ? (
                  <NewProviderPanel
                    preset={cloudPreset}
                    registryModels={registryModels}
                    registrySet={registrySet}
                    onModelsAdded={refreshRegistry}
                  />
                ) : (
                  <p style={{ padding: 16, color: "var(--text-secondary, #888)", fontSize: 13 }}>
                    {t("modelPicker.connect.selectProvider")}
                  </p>
                )}
              </div>

              {/* Pick + use from registry */}
              {registryModels.length > 0 && (
                <div style={{ marginTop: 16 }}>
                  <p style={{ margin: "0 0 8px", fontSize: 13, fontWeight: 600 }}>
                    {t("modelPicker.connect.useModel")}
                  </p>
                  <div style={{ display: "flex", gap: 8 }}>
                    <select
                      className="input"
                      value={pickedRef}
                      onChange={(e) => setPickedRef(e.target.value)}
                      style={{ flex: 1 }}
                    >
                      <option value="">{t("modelPicker.connect.pickPlaceholder")}</option>
                      {registryModels.map((m) => (
                        <option key={m.ref_name} value={m.ref_name}>
                          {m.ref_name} ({m.provider}){m.billing ? ` · ${t(billingKey(m.billing))}` : ""}
                        </option>
                      ))}
                    </select>
                    <button
                      className="btn btn--primary"
                      onClick={handleUseModel}
                      disabled={!pickedRef.trim() || useRefDone}
                    >
                      {useRefDone ? t("modelPicker.connect.used") : t("modelPicker.connect.use")}
                    </button>
                  </div>
                  {useRefError && (
                    <p style={{ margin: "6px 0 0", fontSize: 12, color: "var(--color-error, #f44336)" }}>
                      {useRefError}
                    </p>
                  )}
                </div>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}

// ── Sub-components (use useT() directly to avoid t-prop type complexity) ───

function TabBtn({
  active,
  onClick,
  label,
}: {
  active: boolean;
  onClick: () => void;
  label: string;
}) {
  return (
    <button
      onClick={onClick}
      style={{
        background: "none",
        border: "none",
        padding: "8px 12px",
        fontSize: 13,
        fontWeight: active ? 600 : 400,
        color: active ? "var(--accent, #3b82f6)" : "var(--text-secondary, #888)",
        borderBottom: active ? "2px solid var(--accent, #3b82f6)" : "2px solid transparent",
        cursor: "pointer",
        marginBottom: -1,
      }}
    >
      {label}
    </button>
  );
}

function LocalDownloadSection({
  downloading,
  downloadDone,
  downloadError,
  prog,
  progressDone,
  progressTotal,
  onStart,
  onRetry,
}: {
  downloading: boolean;
  downloadDone: boolean;
  downloadError: string | null;
  prog: ReturnType<typeof downloadProgress>;
  progressDone: number;
  progressTotal: number;
  onStart: () => void;
  onRetry: () => void;
}) {
  const { t } = useT();
  return (
    <div>
      <div
        style={{
          padding: 16,
          borderRadius: 8,
          border: "1px solid var(--border-line)",
          background: "var(--surface-card)",
        }}
      >
        <p style={{ margin: "0 0 8px", fontWeight: 600, fontSize: 14 }}>
          {t("modelPicker.local.title")}
        </p>
        <p style={{ margin: "0 0 14px", fontSize: 13, color: "var(--text-secondary, #888)" }}>
          {t("modelPicker.local.body")}
        </p>

        {!downloading && !downloadDone && !downloadError && (
          <button className="btn btn--primary" onClick={onStart}>
            {t("modelPicker.local.download")}
          </button>
        )}

        {downloading && (
          <div>
            <div
              style={{
                height: 6,
                borderRadius: 3,
                background: "var(--border-line)",
                overflow: "hidden",
                marginBottom: 6,
              }}
            >
              {prog.indeterminate ? (
                <div
                  style={{
                    width: "40%",
                    height: "100%",
                    background: "var(--accent, #3b82f6)",
                    animation: "mp-indeterminate 1.4s infinite linear",
                  }}
                />
              ) : (
                <div
                  style={{
                    width: `${prog.percent}%`,
                    height: "100%",
                    background: "var(--accent, #3b82f6)",
                    transition: "width 0.3s ease",
                  }}
                />
              )}
            </div>
            <p style={{ margin: 0, fontSize: 12, color: "var(--text-secondary, #888)" }}>
              {prog.indeterminate
                ? t("modelPicker.local.downloading")
                : t("modelPicker.local.downloadingPct", {
                    pct: prog.percent,
                    done: Math.round(progressDone / 1_000_000),
                    total: Math.round(progressTotal / 1_000_000),
                  })}
            </p>
          </div>
        )}

        {downloadError && (
          <div>
            <p style={{ margin: "0 0 8px", fontSize: 13, color: "var(--color-error, #f44336)" }}>
              {t("modelPicker.local.error", { error: downloadError })}
            </p>
            <button className="btn btn--secondary" onClick={onRetry}>
              {t("modelPicker.local.retry")}
            </button>
          </div>
        )}

        {downloadDone && (
          <p style={{ margin: 0, fontSize: 13, color: "var(--color-success, #4caf50)" }}>
            {t("modelPicker.local.done")}
          </p>
        )}
      </div>

      <style>{`
        @keyframes mp-indeterminate {
          0%   { transform: translateX(-100%); }
          100% { transform: translateX(350%); }
        }
      `}</style>
    </div>
  );
}
