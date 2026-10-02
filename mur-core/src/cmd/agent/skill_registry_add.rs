//! Install a registry skill onto a specific agent — per-agent sibling of
//! `mcp_registry::cmd_mcp_registry_add`. Reuses the existing git registry +
//! resolver + per-agent installer, adding verify-on-install + Sandboxed trust.
//!
//! Public entry points are consumed by the CLI (Task 3) and Hub GUI; they are
//! wired there, so `dead_code` is expected here.

use std::path::Path;

use anyhow::{Result, bail};
use semver::Version;

use super::skill_signer_trust::{DriftDecision, SignerTrust, check_drift, classify_signer};
use super::skill_verify::{HashStatus, SignatureStatus, VerifyOutcome, verify_skill_install};
use crate::cmd::skill_registry;
use mur_common::skill::loader::is_valid_skill_name;
use mur_common::skill::publisher_trust::PublisherKeyring;
use mur_common::skill::{parse_canonical, scan::scan_skill};

// ─── Consent / view types ──────────────────────────────────────────────────

/// Full consent bundle shown to the user before install.
/// Serialisable so the Hub can display it in its modal.
#[allow(dead_code)] // wired by the CLI/Hub units (Task 3 / Hub PR)
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConsentInfo {
    pub name: String,
    pub version: String,
    pub publisher: String,
    /// String form of `RegistrySkillEntry.category` (already a plain string in the index).
    pub category: String,
    pub signature: SigView,
    /// `"match"` | `"mismatch"` | `"absent"`
    pub hash: String,
    pub mcp_requirements: Vec<String>,
    /// Human-readable findings from `ContentScanReport::human_summary()`.
    pub findings: Vec<String>,
    /// Hard failure — hash Mismatch or invalid signature (proven tampering).
    /// Abort unconditionally; NOT overridable by `--yes`/`accept`.
    pub blocking: bool,
    /// Not proven-bad but not proven-good — requires `--yes` acknowledgement.
    /// Covers: unsigned, absent-hash.
    pub needs_ack: bool,
    /// Content-scan has blocking findings (tool-poisoning / injection / secret /
    /// executable). Requires `--yes` acknowledgement (ack gate, not verify gate).
    pub scan_blocking: bool,
    /// Trust level that will be applied on install (always `"sandboxed"`).
    pub trust_level: String,
    /// Publisher keyring classification of the signer:
    /// `"trusted"` | `"untrusted"` | `"revoked"` | `"unsigned"` | `"invalid"`.
    pub signer_trust: String,
    /// Raw YAML body of the resolved skill file (for Hub preview / consent display).
    pub body: String,
    /// SHA-256 hex of the resolved skill YAML **file bytes**. This is the value
    /// the registry index carries and `verify_skill_install` compares against —
    /// a transport-integrity check ("did I get the exact file the index
    /// promised"). NOT the trust-store key; see `trust_sha256`.
    pub resolved_sha256: String,
    /// SHA-256 hex in the TRUST domain (`content_hash_for_trust`): canonical
    /// YAML with `transfer_chain` / `evolution_log` excluded. This is what the
    /// trust store keys and compares on, so it must be the value pinned as the
    /// drift baseline — `resolved_sha256` is a different hash of a different
    /// thing and comparing the two reports drift on every install.
    pub trust_sha256: String,
    /// Short human description of detected drift since the last install:
    /// `"content changed"`, `"publisher changed"`, `"downgrade X → Y"`, or `None`.
    /// When set, `needs_ack` is also `true` (in `resolve_consent`; not in `resolve_consent_in`
    /// which has no trust store).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift: Option<String>,
}

/// Serialisable view of `SignatureStatus`.
#[allow(dead_code)] // wired by the CLI/Hub units
#[derive(Debug, Clone, serde::Serialize)]
pub struct SigView {
    /// `"verified"` | `"unsigned"` | `"invalid"`
    pub status: String,
    pub publisher: String,
    pub key_fp: String,
}

/// Serialisable registry entry view for the Hub search panel.
#[allow(dead_code)] // wired by the CLI/Hub units
#[derive(Debug, Clone, serde::Serialize)]
pub struct RegistrySkillEntryView {
    pub name: String,
    pub description: String,
    pub publisher: String,
    pub category: String,
    pub latest: String,
    /// `true` when `content_sha256` in the index is non-empty.
    pub signed_in_index: bool,
}

// ─── Internal converters ──────────────────────────────────────────────────

fn hash_str(h: &HashStatus) -> &'static str {
    match h {
        HashStatus::Match => "match",
        HashStatus::Mismatch => "mismatch",
        HashStatus::Absent => "absent",
    }
}

fn sig_view(s: &SignatureStatus) -> SigView {
    match s {
        SignatureStatus::Verified { publisher, key_fp } => SigView {
            status: "verified".into(),
            publisher: publisher.clone(),
            key_fp: key_fp.clone(),
        },
        SignatureStatus::Unsigned => SigView {
            status: "unsigned".into(),
            publisher: String::new(),
            key_fp: String::new(),
        },
        SignatureStatus::Invalid => SigView {
            status: "invalid".into(),
            publisher: String::new(),
            key_fp: String::new(),
        },
    }
}

// ─── Install gate (pure, testable) ───────────────────────────────────────

/// Two-tier fail-closed install gate.
///
/// - Tier 1 (`blocking`): hash Mismatch or invalid signature — proven tampering.
///   Abort unconditionally; `accept` does NOT override this.
/// - Tier 2 (`needs_ack || scan_blocking`): unsigned / absent-hash / scan findings.
///   Requires explicit `accept` (`--yes`).
pub fn gate(consent: &ConsentInfo, accept: bool) -> Result<()> {
    if consent.blocking {
        bail!(
            "skill '{}' failed verify-on-install (hash={}, signature={}) — proven tampering, install refused.",
            consent.name,
            consent.hash,
            consent.signature.status
        );
    }
    if (consent.needs_ack || consent.scan_blocking) && !accept {
        bail!(
            "'{}' needs review (signature={}, hash={}{}); pass --yes to install anyway.",
            consent.name,
            consent.signature.status,
            consent.hash,
            if consent.scan_blocking {
                ", security-scan findings"
            } else {
                ""
            }
        );
    }
    Ok(())
}

// ─── TEST SEAM — takes registry dir directly, no git/network ─────────────

/// Resolve a skill from a local registry directory into a `ConsentInfo`
/// (hash-check + signature-verify + content-scan). Does **not** install.
///
/// `keyring` is the caller's publisher trust keyring; signer trust is folded
/// into `blocking` (Revoked → unconditional block) and `needs_ack` (Untrusted
/// → requires `--yes`).
///
/// This is the network-free test seam. `resolve_consent` wraps it after
/// calling `fetch_and_load` to obtain the registry dir.
pub fn resolve_consent_in(
    registry_dir: &Path,
    skill: &str,
    version: Option<&str>,
    keyring: &PublisherKeyring,
) -> Result<ConsentInfo> {
    // Fix D: reject index-controlled path traversal in skill name.
    if !is_valid_skill_name(skill) {
        bail!("invalid skill name '{skill}' — must be a safe identifier (no path components)");
    }

    let idx = skill_registry::load_index(registry_dir)?;
    let entry = idx
        .skills
        .get(skill)
        .ok_or_else(|| anyhow::anyhow!("skill '{skill}' not found in registry"))?;

    let ver = match version {
        Some(v) => v.to_string(),
        None => entry.latest.clone(),
    };

    // Fix D: reject version strings that contain path traversal characters.
    if ver.contains('/') || ver.contains('\\') || ver.contains("..") {
        bail!("invalid version '{ver}' — must not contain '/', '\\\\', or '..'");
    }

    // Version existence check. When `ver == entry.latest` we allow the
    // versions-dir to be absent (caller may omit it in minimal registries).
    let avail = skill_registry::available_versions(registry_dir, skill)?;
    if !avail.iter().any(|v| v.to_string() == ver) && ver != entry.latest {
        bail!("version '{ver}' of '{skill}' not in registry (available: {avail:?})");
    }

    let _ = Version::parse(&ver); // tolerate non-semver — just a sanity log hook

    let path = skill_registry::skill_yaml_path(registry_dir, skill, &ver);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;

    let manifest =
        parse_canonical(&text).map_err(|e| anyhow::anyhow!("parse skill manifest: {e}"))?;

    // Compute the actual sha256 of the resolved file for drift-pinning.
    let resolved_sha256 = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(text.as_bytes()))
    };

    // Trust-domain hash — what the trust store keys and compares on. Distinct
    // from `resolved_sha256` above: that one is over the raw file bytes for the
    // index check, this one is over the canonical manifest.
    let trust_sha256 = mur_common::skill::content_hash_for_trust(&manifest)
        .map_err(|e| anyhow::anyhow!("trust hash: {e}"))?;

    let outcome: VerifyOutcome = verify_skill_install(&manifest, &text, &entry.content_sha256);
    let signer = classify_signer(&outcome.signature, keyring);
    // Fold publisher keyring trust into the gate flags:
    // - Revoked → unconditional block (proven unsafe signer).
    // - Untrusted → requires --yes (signature valid but signer not in keyring).
    let blocking = outcome.is_blocking() || matches!(signer, SignerTrust::Revoked);
    let needs_ack = outcome.needs_ack() || matches!(signer, SignerTrust::Untrusted);
    let report = scan_skill(&manifest).map_err(|e| anyhow::anyhow!("content scan: {e}"))?;
    let scan_blocking = report.has_blocking_findings();

    Ok(ConsentInfo {
        name: manifest.name.clone(),
        version: ver,
        publisher: entry.publisher.clone(),
        category: entry.category.clone(),
        signature: sig_view(&outcome.signature),
        hash: hash_str(&outcome.hash).into(),
        mcp_requirements: manifest
            .mcp_requirements
            .iter()
            .map(|r| format!("{} ({})", r.tool_pattern, r.capability.as_str()))
            .collect(),
        // ContentScanReport has no flat `findings` field — use human_summary().
        findings: report.human_summary(),
        blocking,
        needs_ack,
        scan_blocking,
        trust_level: "sandboxed".to_string(),
        signer_trust: signer.as_str().to_string(),
        body: text,
        resolved_sha256,
        trust_sha256,
        // Drift is computed only by resolve_consent (has mur_home) and
        // cmd_skill_registry_add; left None here (test seam has no trust store).
        drift: None,
    })
}

// ─── Drift helper (shared by resolve_consent + cmd_skill_registry_add) ──────

/// Load the trust store and check for drift against the prior install.
///
/// Returns `(description, decision)`:
/// - `description` — a short human string (`"content changed"`, `"publisher changed"`,
///   `"downgrade X → Y"`), or `None` for first install or no drift.
/// - `decision` — the raw `DriftDecision` for control flow in `cmd_skill_registry_add`.
///
/// Uses `entries.get(name)` — registry-add pins its entry keyed by name; other install
/// paths (`mur skill install` etc.) write hash-keyed entries. Iterating `.values()` and
/// matching on `.name` may return a stale hash-keyed entry whose empty `content_sha256`
/// makes `check_drift` skip both comparisons → silent rug-pull (C1 fix).
fn drift_status(
    mur_home: &Path,
    name: &str,
    new_hash: &str,
    new_signer: Option<&str>,
    new_ver: &str,
) -> (Option<String>, DriftDecision) {
    let trust_store =
        mur_common::trust::skills::SkillTrustStore::load(mur_home).unwrap_or_default();
    // Registry-add pins by name; other paths key by hash — look up by name.
    let prior = trust_store.entries.get(name);
    let prior_tuple = prior.map(|e| {
        (
            e.content_sha256.as_str(),
            e.signer_key_fp.as_deref(),
            e.version.as_str(),
        )
    });
    let decision = check_drift(prior_tuple, new_hash, new_signer, new_ver);
    let description = match &decision {
        DriftDecision::None => None,
        DriftDecision::Changed { what } => Some(format!("{what} changed")),
        DriftDecision::Rollback { installed, offered } => {
            Some(format!("downgrade {installed} → {offered}"))
        }
    };
    (description, decision)
}

// ─── Public entry points ───────────────────────────────────────────────────

/// Fetch the registry (git) then resolve consent. Hub preview + CLI confirm
/// both call this; neither installs until the gate passes.
///
/// Drift against a prior install is computed here (we have `mur_home`), folded
/// into `needs_ack`, and surfaced as `consent.drift` for the Hub modal.
#[allow(dead_code)] // wired by the CLI/Hub units
pub fn resolve_consent(mur_home: &Path, skill: &str, version: Option<&str>) -> Result<ConsentInfo> {
    let (dir, _idx) = skill_registry::fetch_and_load(mur_home, skill_registry::DEFAULT_REGISTRY)?;
    let keyring = PublisherKeyring::load_or_seed(mur_home)?;
    let mut consent = resolve_consent_in(&dir, skill, version, &keyring)?;

    // Compute drift and surface it so the Hub accept-checkbox appears on updates.
    let new_signer_fp = if consent.signature.status == "verified" {
        Some(consent.signature.key_fp.clone())
    } else {
        None
    };
    let (drift_desc, _) = drift_status(
        mur_home,
        &consent.name,
        // Trust domain on both sides: the stored baseline is `trust_sha256`,
        // so comparing `resolved_sha256` (raw file bytes) here would report
        // "content changed" on every single install.
        &consent.trust_sha256,
        new_signer_fp.as_deref(),
        &consent.version,
    );
    if drift_desc.is_some() {
        consent.needs_ack = true;
    }
    consent.drift = drift_desc;
    Ok(consent)
}

/// Install a registry skill onto a specific agent at `TrustLevel::Sandboxed`.
///
/// Fail-closed gate:
/// - `blocking`  → abort unconditionally (hash Mismatch or invalid signature).
/// - `(needs_ack || scan_blocking) && !accept` → abort with a `--yes` hint.
///
/// On success returns the skill path relative to the agent home (`"skills/<name>"`).
#[allow(dead_code)] // wired by the CLI/Hub units
pub async fn cmd_skill_registry_add(
    agent: &str,
    skill: &str,
    version: Option<&str>,
    accept: bool,
) -> Result<String> {
    let mur_home = super::resolve_mur_home()?;
    // Keep `dir` in scope so the already-resolved file stays on disk while we
    // pass its path to `cmd_skill_add` — no redundant temp copy needed.
    let (dir, _idx) = skill_registry::fetch_and_load(&mur_home, skill_registry::DEFAULT_REGISTRY)?;
    let keyring = PublisherKeyring::load_or_seed(&mur_home)?;
    let consent = resolve_consent_in(&dir, skill, version, &keyring)?;

    // ── Rug-pull / rollback: compare against any prior install record ────────
    // Use drift_status (C1 fix): registry-add pins by name; other paths key by
    // hash. entries.get(name) avoids returning a stale hash-keyed entry whose
    // empty content_sha256 would make check_drift skip the comparison → silent rug-pull.
    let new_signer_fp = if consent.signature.status == "verified" {
        Some(consent.signature.key_fp.clone())
    } else {
        None
    };
    let (_, drift_decision) = drift_status(
        &mur_home,
        &consent.name,
        // Trust domain on both sides: the stored baseline is `trust_sha256`,
        // so comparing `resolved_sha256` (raw file bytes) here would report
        // "content changed" on every single install.
        &consent.trust_sha256,
        new_signer_fp.as_deref(),
        &consent.version,
    );
    match drift_decision {
        DriftDecision::None => {}
        DriftDecision::Changed { what } => {
            if !accept {
                anyhow::bail!(
                    "skill '{}' has changed {} since last install; pass --yes to accept the update.",
                    consent.name,
                    what
                );
            }
        }
        DriftDecision::Rollback { installed, offered } => {
            if !accept {
                anyhow::bail!(
                    "skill '{}' downgrade refused (installed={installed}, offered={offered}); pass --yes to force.",
                    consent.name
                );
            }
        }
    }

    gate(&consent, accept)?;

    let path = skill_registry::skill_yaml_path(&dir, skill, &consent.version);
    super::skill::cmd_skill_add(agent, &path.to_string_lossy())?;

    // Pin content hash + signer key in the trust store for future drift detection.
    {
        use mur_common::skill::TrustLevel;
        use mur_common::trust::skills::{SkillTrustStore, TrustEntry};
        let mut ts = SkillTrustStore::load(&mur_home).unwrap_or_default();
        // Key the entry by the skill name so we can find it by name on the next
        // install (the hash-keyed lookup is for load-time allow-listing; the
        // name-keyed find is for drift detection).
        ts.entries.insert(
            consent.name.clone(),
            TrustEntry {
                name: consent.name.clone(),
                version: consent.version.clone(),
                level: TrustLevel::Sandboxed,
                installed_at: chrono::Utc::now().to_rfc3339(),
                publisher: if consent.publisher.is_empty() {
                    None
                } else {
                    Some(consent.publisher.clone())
                },
                content_sha256: consent.trust_sha256.clone(),
                signer_key_fp: if consent.signature.key_fp.is_empty() {
                    None
                } else {
                    Some(consent.signature.key_fp.clone())
                },
            },
        );
        ts.save(&mur_home)
            .map_err(|e| anyhow::anyhow!("save trust store: {e}"))?;
    }

    Ok(format!("skills/{}", consent.name))
}

/// Search the registry and return serialisable views (Hub search panel + CLI).
#[allow(dead_code)] // wired by the CLI/Hub units
pub fn registry_search_for_agent(
    mur_home: &Path,
    query: &str,
) -> Result<Vec<RegistrySkillEntryView>> {
    let (_dir, idx) = skill_registry::fetch_and_load(mur_home, skill_registry::DEFAULT_REGISTRY)?;
    Ok(skill_registry::search_registry(&idx, query)
        .into_iter()
        .map(|(name, e)| RegistrySkillEntryView {
            name: name.to_string(),
            description: e.description.clone(),
            publisher: e.publisher.clone(),
            category: e.category.clone(),
            latest: e.latest.clone(),
            signed_in_index: !e.content_sha256.is_empty(),
        })
        .collect())
}

// ─── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
