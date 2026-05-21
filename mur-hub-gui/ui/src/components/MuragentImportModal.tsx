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

interface Props {
  isOpen: boolean;
  onClose: () => void;
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

const FIRST_TIME_AUTHOR_DELAY_MS = 5_000;

export function MuragentImportModal({ isOpen, onClose }: Props) {
  const [path, setPath] = useState<string | null>(null);
  const [inspection, setInspection] = useState<MuragentInspection | null>(null);
  const [installing, setInstalling] = useState(false);
  const [receipt, setReceipt] = useState<InstallReceipt | null>(null);
  const [error, setError] = useState<string | null>(null);
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
      setDelayRemaining(-1);
    }
  }, [isOpen]);

  // 5-second delay tick for first-time-author imports (§7.2 rule 4)
  useEffect(() => {
    if (delayRemaining <= 0) return;
    const id = setInterval(() => {
      setDelayRemaining((r) => Math.max(0, r - 100));
    }, 100);
    return () => clearInterval(id);
  }, [delayRemaining]);

  if (!isOpen) return null;

  async function chooseFile() {
    setError(null);
    setReceipt(null);
    setInspection(null);
    try {
      const selected = await open({
        multiple: false,
        filters: [{ name: "MuR Agent Package", extensions: ["muragent"] }],
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
    } catch (e) {
      setError(String(e));
    } finally {
      setInstalling(false);
    }
  }

  function importDisabledReason(): string | null {
    if (!inspection) return "Select a file first";
    if (!inspection.signature_valid) return "Signature is invalid";
    if (inspection.trust_status.kind === "key_change_refused") return "Refused: key change without rotation";
    if (delayRemaining > 0) {
      return `Wait ${Math.ceil(delayRemaining / 1000)}s — please review the permissions`;
    }
    if (installing) return "Installing…";
    return null;
  }

  const disabledReason = useMemo(importDisabledReason, [
    inspection,
    delayRemaining,
    installing,
  ]);
  const importDisabled = disabledReason !== null;

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div
        className="modal-panel"
        style={{ maxWidth: 520 }}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-header">
          <h2>Import Agent</h2>
          <button className="modal-close" onClick={onClose}>×</button>
        </div>

        <div className="modal-body">
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

          {receipt && <ReceiptStep receipt={receipt} />}
        </div>

        {inspection && !receipt && (
          <div
            className="modal-footer"
            style={{
              display: "flex",
              justifyContent: "flex-end",
              gap: 8,
              padding: "12px 16px",
              borderTop: "1px solid var(--border, #2a2a2a)",
            }}
          >
            <button className="toolbar-btn" onClick={onClose}>
              Cancel
            </button>
            <button
              className="toolbar-btn primary"
              onClick={confirmImport}
              disabled={importDisabled}
              title={disabledReason ?? "Import this agent"}
            >
              {installing
                ? "Installing…"
                : delayRemaining > 0
                ? `Import (${Math.ceil(delayRemaining / 1000)}s)`
                : "Import"}
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
  return (
    <div>
      <p style={{ marginBottom: 12, color: "var(--text-secondary, #888)", fontSize: 13 }}>
        Select a <code>.muragent</code> file to inspect and (optionally) install.
        The package will be validated before any data is written.
      </p>
      <button className="toolbar-btn" onClick={onChoose}>
        Choose File…
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
          <strong>Refused — signature or integrity check failed.</strong>
          <div style={{ marginTop: 4, fontFamily: "var(--font-mono, monospace)", fontSize: 11 }}>
            {inspection.signature_error ?? "unknown error"}
          </div>
        </div>
      )}

      {/* Fingerprint (spec §7.2 rule 2) */}
      {inspection.signature_valid && (
        <div style={{ marginBottom: 12, fontSize: 12 }}>
          <div style={{ color: "var(--text-secondary, #888)", marginBottom: 2 }}>
            Author fingerprint
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
          Please review the permissions above. Import will be enabled in{" "}
          {Math.ceil(delayRemaining / 1000)}s.
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
        First time you've imported anything from this author.
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
      Refused — signing key changed for a known author with no rotation manifest.
      <div style={{ marginTop: 4, fontFamily: "var(--font-mono, monospace)", fontSize: 11 }}>
        previous fingerprint: {ts.previous_fingerprint}
      </div>
    </div>
  );
}

function PermissionsList({ permissions }: { permissions: DeclaredPermissions }) {
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
        This agent will be allowed to:
      </div>
      {empty && (
        <div style={{ fontSize: 13, fontStyle: "italic", color: "var(--text-secondary, #888)" }}>
          (no declared permissions)
        </div>
      )}
      {permissions.mcp_servers.length > 0 && (
        <Detail label="Spawn MCP servers">
          {permissions.mcp_servers.map((m) => (
            <div key={m.name} style={{ fontFamily: "var(--font-mono, monospace)" }}>
              {m.name} <span style={{ opacity: 0.6 }}>({m.command_basename})</span>
            </div>
          ))}
        </Detail>
      )}
      {permissions.capabilities.length > 0 && (
        <Detail label="Capabilities">
          <div style={{ fontFamily: "var(--font-mono, monospace)" }}>
            {permissions.capabilities.join(", ")}
          </div>
        </Detail>
      )}
      {permissions.voice_enabled && <Detail label="Use microphone + speakers" />}
      {permissions.pet_enabled && <Detail label="Spawn a pet window" />}
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
  const verb = receipt.was_update ? "Updated" : "Installed";
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
        Trust: <code>{receipt.trust_level}</code> · Fingerprint{" "}
        <code>{receipt.fingerprint_hex}</code>
      </div>
    </div>
  );
}
