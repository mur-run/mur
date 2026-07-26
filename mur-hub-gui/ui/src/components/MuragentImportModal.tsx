//! §7.2 first-time-author import dialog for `.muragent` files.
//!
//! Two-step flow:
//! 1. File picker → invoke `inspect_muragent_file` (read-only).
//! 2. Render confirmation with display name, fingerprint (hex + 4-word),
//!    declared permissions, and a trust badge. Import button is disabled
//!    for 5 seconds on first-time-author per spec §7.2 rule 4.
//!
//! Signature/integrity failures are surfaced as a refusal — no Import
//! button is offered (spec §7.5: no click-through on invalid signatures).

import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useT } from "../i18n";

interface Props {
  isOpen: boolean;
  onClose: () => void;
  initialPath?: string;
}

interface McpServerView {
  name: string;
  command_basename: string;
}

interface DeclaredPermissions {
  mcp_servers: McpServerView[];
  capabilities: string[];
  voice_enabled: boolean;
  pet_enabled: boolean;
}

type TrustStatus =
  | { kind: "first_time_author" }
  | { kind: "known_author"; level: string }
  | { kind: "key_change_refused"; previous_fingerprint: string };

interface MuragentInspection {
  display_name: string;
  slug: string;
  bundle_id: string;
  url_scheme: string;
  original_uuid: string;
  schema: string;
  mur_version: string;
  exported_at: string;
  required_surfaces: string[];
  signature_valid: boolean;
  signature_error: string | null;
  trust_status: TrustStatus;
  fingerprint_hex: string;
  fingerprint_words: string;
  author_keyid: string;
  permissions: DeclaredPermissions;
}

interface InstallReceipt {
  display_name: string;
  slug: string;
  was_update: boolean;
  trust_level: string;
  fingerprint_hex: string;
}

interface ModelHintView {
  provider: string;
  name: string;
  tier: string;
  min_ram_gb: number;
  local_capable: boolean;
}

interface ResolutionView {
  hint: ModelHintView | null;
  total_ram_gb: number;
  apple_silicon: boolean;
  ollama_present: boolean;
  recommendation: "local" | "cloud" | "cloud_or_smaller_local" | "neutral_menu";
}

interface ModelChoice {
  provider: string;
  model: string;
  base_url?: string;
  secret?: string;
}

const FIRST_TIME_AUTHOR_DELAY_MS = 5_000;

export function MuragentImportModal({ isOpen, onClose, initialPath }: Props) {
  const { t } = useT();
  const [path, setPath] = useState<string | null>(null);
  const [inspection, setInspection] = useState<MuragentInspection | null>(null);
  const [installing, setInstalling] = useState(false);
  const [receipt, setReceipt] = useState<InstallReceipt | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [resolutionView, setResolutionView] = useState<ResolutionView | null>(null);
  const [modelApplied, setModelApplied] = useState(false);
  const [modelError, setModelError] = useState<string | null>(null);
  // Counts down from FIRST_TIME_AUTHOR_DELAY_MS to 0 for first-time authors.
  // Known authors get 0 immediately. -1 means no inspection loaded yet.
  const [delayRemaining, setDelayRemaining] = useState<number>(-1);

  // Reset all state when modal is reopened
  useEffect(() => {
    if (!isOpen) {
      setPath(null);
      setInspection(null);
      setInstalling(false);
      setReceipt(null);
      setError(null);
      setResolutionView(null);
      setModelApplied(false);
      setModelError(null);
      setDelayRemaining(-1);
    }
  }, [isOpen]);

  // Auto-inspect when a file path is provided via initialPath (e.g. from OS file open)
  useEffect(() => {
    if (!isOpen || !initialPath) return;
    setPath(initialPath);
    setError(null);
    setReceipt(null);
    invoke<MuragentInspection>("inspect_muragent_file", { path: initialPath })
      .then((result) => {
        setInspection(result);
        if (result.signature_valid && result.trust_status.kind === "first_time_author") {
          setDelayRemaining(FIRST_TIME_AUTHOR_DELAY_MS);
        } else {
          setDelayRemaining(0);
        }
      })
      .catch((e) => setError(String(e)));
  }, [isOpen, initialPath]);

  // 5-second delay tick for first-time-author imports (§7.2 rule 4)
  useEffect(() => {
    if (delayRemaining <= 0) return;
    const id = setInterval(() => {
      setDelayRemaining((r) => Math.max(0, r - 100));
    }, 100);
    return () => clearInterval(id);
  }, [delayRemaining]);

  // NOTE: all hooks must run before any early return. `importDisabledReason` is a
  // hoisted function declaration below, so it can be referenced here. Keeping this
  // useMemo above `if (!isOpen) return null` avoids React error #310 (hook count
  // changing between isOpen=false/true renders), which blanked the whole UI.
  const disabledReason = useMemo(importDisabledReason, [
    inspection,
    delayRemaining,
    installing,
    t,
  ]);
  const importDisabled = disabledReason !== null;

  if (!isOpen) return null;

  async function chooseFile() {
    setError(null);
    setReceipt(null);
    setInspection(null);
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: "MUR Agent Package", extensions: ["muragent"] }],
      });
      if (!selected) return;
      const chosen = typeof selected === "string" ? selected : selected[0];
      setPath(chosen);
      const result = await invoke<MuragentInspection>("inspect_muragent_file", {
        path: chosen,
      });
      setInspection(result);
      // First-time authors must wait the full delay; known authors can import immediately.
      if (result.signature_valid && result.trust_status.kind === "first_time_author") {
        setDelayRemaining(FIRST_TIME_AUTHOR_DELAY_MS);
      } else {
        setDelayRemaining(0);
      }
    } catch (e) {
      setError(String(e));
    }
  }

  async function confirmImport() {
    if (!path || !inspection) return;
    setInstalling(true);
    setError(null);
    try {
      const r = await invoke<InstallReceipt>("install_muragent_file", { path });
      setReceipt(r);
      // Fetch model resolution info for the wizard step
      try {
        if (path) {
          const rv = await invoke<ResolutionView>("model_resolution_view", { path });
          // Only show the wizard if there's useful info (hint present or non-neutral recommendation)
          if (rv.hint !== null || rv.recommendation !== "neutral_menu") {
            setResolutionView(rv);
          }
        }
      } catch {
        // model_resolution_view is best-effort; failure just skips the wizard step
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setInstalling(false);
    }
  }

  async function applyModelChoice(choice: ModelChoice) {
    if (!receipt) return;
    setModelError(null);
    try {
      await invoke<string>("apply_agent_model", { slug: receipt.slug, choice });
      setModelApplied(true);
    } catch (e) {
      setModelError(String(e));
    }
  }

  function importDisabledReason(): string | null {
    if (!inspection) return t("modal.import.disabled.selectFirst");
    if (!inspection.signature_valid) return t("modal.import.disabled.signatureInvalid");
    if (inspection.trust_status.kind === "key_change_refused")
      return t("modal.import.disabled.keyChange");
    if (delayRemaining > 0) {
      return t("modal.import.disabled.waitReview", {
        seconds: Math.ceil(delayRemaining / 1000),
      });
    }
    if (installing) return t("modal.import.installing");
    return null;
  }

  return (
    <div className="modal__overlay" onClick={onClose}>
      <div
        className="modal"
        style={{ width: 520 }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal__header">
          <h2 className="modal__title">{t("modal.import.title")}</h2>
          <button className="modal__close" onClick={onClose}>
            ×
          </button>
        </div>

        <div className="modal__body">
          {!inspection && !receipt && (
            <ChooseFileStep onChoose={chooseFile} error={error} />
          )}

          {inspection && !receipt && (
            <ReviewStep
              inspection={inspection}
              delayRemaining={delayRemaining}
              error={error}
            />
          )}

          {receipt && !resolutionView && <ReceiptStep receipt={receipt} />}
          {receipt && resolutionView && !modelApplied && (
            <>
              <ReceiptStep receipt={receipt} />
              <hr style={{ margin: "12px 0", borderColor: "var(--border, #2a2a2a)" }} />
              <ModelResolutionStep
                receipt={receipt}
                view={resolutionView}
                applied={modelApplied}
                error={modelError}
                onApply={applyModelChoice}
                onSkip={() => setResolutionView(null)}
              />
            </>
          )}
          {receipt && resolutionView && modelApplied && (
            <ModelResolutionStep
              receipt={receipt}
              view={resolutionView}
              applied={true}
              error={null}
              onApply={() => {}}
              onSkip={() => {}}
            />
          )}
        </div>

        {inspection && !receipt && (
          <div className="modal__footer">
            <button className="btn btn--secondary" onClick={onClose}>
              {t("modal.import.cancel")}
            </button>
            <button
              className="btn btn--primary"
              onClick={confirmImport}
              disabled={importDisabled}
              title={disabledReason ?? t("modal.import.importTooltip")}
            >
              {installing
                ? t("modal.import.installing")
                : delayRemaining > 0
                ? t("modal.import.importTimed", {
                    seconds: Math.ceil(delayRemaining / 1000),
                  })
                : t("modal.import.import")}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

// ─── Step 1: choose file ──────────────────────────────────────────────────

function ChooseFileStep({
  onChoose,
  error,
}: {
  onChoose: () => void;
  error: string | null;
}) {
  const { t } = useT();
  return (
    <div>
      <p style={{ marginBottom: 12, color: "var(--text-secondary, #888)", fontSize: 13 }}>
        {t("modal.import.choose.body")}
      </p>
      <button className="btn btn--primary" onClick={onChoose}>
        {t("modal.import.choose.button")}
      </button>
      {error && (
        <p style={{ marginTop: 12, color: "var(--color-error, #f44336)", fontSize: 13 }}>
          {error}
        </p>
      )}
    </div>
  );
}

// ─── Step 2: review + confirm ─────────────────────────────────────────────

function ReviewStep({
  inspection,
  delayRemaining,
  error,
}: {
  inspection: MuragentInspection;
  delayRemaining: number;
  error: string | null;
}) {
  const { t } = useT();
  return (
    <div>
      {/* Identity header (spec §7.2 rule 2) */}
      <div style={{ marginBottom: 12 }}>
        <div style={{ fontSize: 18, fontWeight: 600 }}>{inspection.display_name}</div>
        <div
          style={{
            fontSize: 12,
            color: "var(--text-secondary, #888)",
            fontFamily: "var(--font-mono, monospace)",
          }}
        >
          {inspection.slug} · mur {inspection.mur_version}
        </div>
      </div>

      <TrustBadge inspection={inspection} />

      {/* Signature / refusal block */}
      {!inspection.signature_valid && (
        <div
          style={{
            padding: 10,
            marginBottom: 12,
            borderRadius: 6,
            background: "var(--color-error-bg, #2a1010)",
            color: "var(--color-error, #f44336)",
            fontSize: 13,
          }}
        >
          <strong>{t("modal.import.refused.title")}</strong>
          <div style={{ marginTop: 4, fontFamily: "var(--font-mono, monospace)", fontSize: 11 }}>
            {inspection.signature_error ?? t("modal.import.refused.unknownError")}
          </div>
        </div>
      )}

      {/* Fingerprint (spec §7.2 rule 2) */}
      {inspection.signature_valid && (
        <div style={{ marginBottom: 12, fontSize: 12 }}>
          <div style={{ color: "var(--text-secondary, #888)", marginBottom: 2 }}>
            {t("modal.import.review.fingerprint")}
          </div>
          <div style={{ fontFamily: "var(--font-mono, monospace)" }}>
            {inspection.fingerprint_hex} · {inspection.fingerprint_words}
          </div>
        </div>
      )}

      {/* Permissions (spec §7.2 rule 5) */}
      {inspection.signature_valid && (
        <PermissionsList permissions={inspection.permissions} />
      )}

      {/* Delay hint */}
      {delayRemaining > 0 && (
        <p
          style={{
            marginTop: 12,
            fontSize: 12,
            color: "var(--text-secondary, #888)",
            fontStyle: "italic",
          }}
        >
          {t("modal.import.review.delayHint", {
            seconds: Math.ceil(delayRemaining / 1000),
          })}
        </p>
      )}

      {error && (
        <p style={{ marginTop: 12, color: "var(--color-error, #f44336)", fontSize: 13 }}>
          {error}
        </p>
      )}
    </div>
  );
}

function TrustBadge({ inspection }: { inspection: MuragentInspection }) {
  const { t } = useT();
  if (!inspection.signature_valid) return null;
  const ts = inspection.trust_status;
  if (ts.kind === "known_author") {
    // Spec §7.2 table: known authors have no badge — trust is the silent default.
    return null;
  }
  if (ts.kind === "first_time_author") {
    return (
      <div
        style={{
          padding: 8,
          marginBottom: 12,
          borderRadius: 6,
          background: "var(--color-info-bg, #1a2330)",
          color: "var(--color-info, #6aa9ff)",
          fontSize: 12,
        }}
      >
        {t("modal.import.trust.firstTime")}
      </div>
    );
  }
  // key_change_refused
  return (
    <div
      style={{
        padding: 8,
        marginBottom: 12,
        borderRadius: 6,
        background: "var(--color-error-bg, #2a1010)",
        color: "var(--color-error, #f44336)",
        fontSize: 12,
      }}
    >
      {t("modal.import.trust.keyChanged")}
      <div style={{ marginTop: 4, fontFamily: "var(--font-mono, monospace)", fontSize: 11 }}>
        {t("modal.import.trust.previousFingerprint", {
          fingerprint: ts.previous_fingerprint,
        })}
      </div>
    </div>
  );
}

function PermissionsList({ permissions }: { permissions: DeclaredPermissions }) {
  const { t } = useT();
  const empty =
    permissions.mcp_servers.length === 0 &&
    permissions.capabilities.length === 0 &&
    !permissions.voice_enabled &&
    !permissions.pet_enabled;

  return (
    <div style={{ marginBottom: 12 }}>
      <div
        style={{
          fontSize: 12,
          color: "var(--text-secondary, #888)",
          marginBottom: 4,
        }}
      >
        {t("modal.import.review.allowTitle")}
      </div>
      {empty && (
        <div style={{ fontSize: 13, fontStyle: "italic", color: "var(--text-secondary, #888)" }}>
          {t("modal.import.review.noPermissions")}
        </div>
      )}
      {permissions.mcp_servers.length > 0 && (
        <Detail label={t("modal.import.review.spawnMcp")}>
          {permissions.mcp_servers.map((m) => (
            <div key={m.name} style={{ fontFamily: "var(--font-mono, monospace)" }}>
              {m.name} <span style={{ opacity: 0.6 }}>({m.command_basename})</span>
            </div>
          ))}
        </Detail>
      )}
      {permissions.capabilities.length > 0 && (
        <Detail label={t("modal.import.review.capabilities")}>
          <div style={{ fontFamily: "var(--font-mono, monospace)" }}>
            {permissions.capabilities.join(", ")}
          </div>
        </Detail>
      )}
      {permissions.voice_enabled && <Detail label={t("modal.import.review.microphone")} />}
      {permissions.pet_enabled && <Detail label={t("modal.import.review.spawnPet")} />}
    </div>
  );
}

function Detail({
  label,
  children,
}: {
  label: string;
  children?: React.ReactNode;
}) {
  return (
    <div style={{ marginTop: 4, fontSize: 13 }}>
      <div>· {label}</div>
      {children && (
        <div style={{ marginLeft: 14, fontSize: 12, color: "var(--text-secondary, #888)" }}>
          {children}
        </div>
      )}
    </div>
  );
}

// ─── Step 3: receipt ──────────────────────────────────────────────────────

function ReceiptStep({ receipt }: { receipt: InstallReceipt }) {
  const { t } = useT();
  const verb = receipt.was_update
    ? t("modal.import.receipt.updated")
    : t("modal.import.receipt.installed");
  return (
    <div>
      <div
        style={{
          padding: 10,
          marginBottom: 8,
          borderRadius: 6,
          background: "var(--color-success-bg, #102a1a)",
          color: "var(--color-success, #4caf50)",
          fontSize: 13,
        }}
      >
        ✓ {verb} <strong>{receipt.display_name}</strong> ({receipt.slug})
      </div>
      <div style={{ fontSize: 12, color: "var(--text-secondary, #888)" }}>
        {t("modal.import.receipt.trustLabel")} <code>{receipt.trust_level}</code> ·{" "}
        {t("modal.import.receipt.fingerprintLabel")}{" "}
        <code>{receipt.fingerprint_hex}</code>
      </div>
    </div>
  );
}

// ─── Model resolution step (spec §7.3) ──────────────────────────────────────

function ModelResolutionStep({
  receipt,
  view,
  applied,
  error,
  onApply,
  onSkip,
}: {
  receipt: InstallReceipt;
  view: ResolutionView;
  applied: boolean;
  error: string | null;
  onApply: (choice: ModelChoice) => void;
  onSkip: () => void;
}) {
  const { t } = useT();
  const [provider, setProvider] = useState(
    view.recommendation === "local" ? "ollama" : "anthropic"
  );
  const [model, setModel] = useState(
    view.recommendation === "local"
      ? (view.hint?.local_capable ? view.hint.name : "llama3.2:3b")
      : (view.hint?.name ?? "claude-sonnet-5")
  );
  const [secret, setSecret] = useState("");

  if (applied) {
    return (
      <div style={{ padding: "16px 0" }}>
        <p style={{ color: "var(--success, #4caf50)", marginBottom: 8 }}>
          ✅ {t("modal.import.model.configured", { name: receipt.display_name })}
        </p>
        <p style={{ fontSize: 12, color: "var(--muted, #888)" }}>
          {t("modal.import.model.changeHint")}
        </p>
      </div>
    );
  }

  const recLabel: Record<string, string> = {
    local: t("modal.import.model.rec.local"),
    cloud: t("modal.import.model.rec.cloud"),
    cloud_or_smaller_local: t("modal.import.model.rec.cloudOrSmaller"),
    neutral_menu: t("modal.import.model.rec.neutral"),
  };

  return (
    <div style={{ padding: "8px 0" }}>
      <p style={{ fontWeight: 600, marginBottom: 4 }}>{t("modal.import.model.setup")}</p>
      <p style={{ fontSize: 13, color: "var(--muted, #888)", marginBottom: 12 }}>
        {recLabel[view.recommendation]}
        {view.hint && (
          <span>
            {" "}({t("modal.import.model.original")}: <code>{view.hint.provider}/{view.hint.name}</code>)
          </span>
        )}
      </p>

      <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
        <div style={{ display: "flex", gap: 8 }}>
          <div style={{ flex: 1 }}>
            <label style={{ fontSize: 12 }}>{t("modal.import.model.provider")}</label>
            <input
              className="input"
              value={provider}
              onChange={(e) => setProvider(e.target.value)}
              placeholder={t("modal.import.model.providerPlaceholder")}
              style={{ width: "100%", marginTop: 2 }}
            />
          </div>
          <div style={{ flex: 2 }}>
            <label style={{ fontSize: 12 }}>{t("modal.import.model.model")}</label>
            <input
              className="input"
              value={model}
              onChange={(e) => setModel(e.target.value)}
              placeholder={t("modal.import.model.modelPlaceholder")}
              style={{ width: "100%", marginTop: 2 }}
            />
          </div>
        </div>

        {provider !== "ollama" && (
          <div>
            <label style={{ fontSize: 12 }}>{t("modal.import.model.apiKey")}</label>
            <input
              className="input"
              type="password"
              value={secret}
              onChange={(e) => setSecret(e.target.value)}
              placeholder={t("modal.import.model.apiKeyPlaceholder")}
              style={{ width: "100%", marginTop: 2 }}
            />
          </div>
        )}

        {error && (
          <p style={{ color: "var(--error, #f44336)", fontSize: 12 }}>{error}</p>
        )}
      </div>

      <div style={{ display: "flex", justifyContent: "flex-end", gap: 8, marginTop: 12 }}>
        <button className="btn btn--secondary" onClick={onSkip}>
          {t("modal.import.model.skip")}
        </button>
        <button
          className="btn btn--primary"
          onClick={() =>
            onApply({
              provider,
              model,
              secret: secret.trim() || undefined,
            })
          }
          disabled={!provider.trim() || !model.trim()}
        >
          {t("modal.import.model.save")}
        </button>
      </div>
    </div>
  );
}
