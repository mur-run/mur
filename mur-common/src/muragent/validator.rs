//! 11-step validation pipeline (§6.4).
//!
//! Every step's failure is fatal — no "continue anyway" path.

use crate::muragent::MuragentError;
use crate::muragent::dsse;
use crate::muragent::executable_ban;
use crate::muragent::jcs_canonical;
use crate::muragent::manifest::MuragentManifest;
use crate::muragent::reader::MuragentArchive;
use crate::muragent::statement::{InTotoStatement, verify_subjects};

pub struct ValidationResult {
    pub manifest: MuragentManifest,
    pub author_pubkey: [u8; 32],
    pub keyid: String,
}

impl std::fmt::Debug for ValidationResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValidationResult")
            .field("manifest_schema", &self.manifest.schema)
            .field("agent_slug", &self.manifest.agent.slug)
            .field("keyid", &self.keyid)
            .finish()
    }
}

/// Run the full 11-step validation pipeline. Every failure is fatal (§7.5).
pub fn validate(archive: &MuragentArchive) -> Result<ValidationResult, MuragentError> {
    // Step 1: Tarball integrity — already done by MuragentArchive::read

    // Step 2: No executable content
    for path in archive.files.keys() {
        executable_ban::check_extension(path).map_err(MuragentError::ExecutableContent)?;
    }
    let manifest_yaml = archive.get_str("manifest.yaml")?;
    let manifest: MuragentManifest = serde_yaml_ng::from_str(manifest_yaml)
        .map_err(|e| MuragentError::ManifestParse(e.to_string()))?;
    for mcp in &manifest.mcp_servers {
        executable_ban::check_mcp_command(&mcp.command_basename, &[])
            .map_err(MuragentError::ForbiddenMcpCommand)?;
    }

    // Step 3: Schema version
    if !manifest.is_v2() {
        return Err(MuragentError::SchemaMismatch(manifest.schema.clone()));
    }

    // Step 3.5: Bundle ID
    manifest
        .validate_bundle_id()
        .map_err(MuragentError::ManifestParse)?;

    // Step 4: Version compatibility — deferred to caller (Hub/Commander checks its own version)

    // Step 5: manifest.signed.json matches re-derived canonical JSON
    let embedded_signed_json = archive
        .get("manifest.signed.json")
        .ok_or_else(|| MuragentError::Other("missing manifest.signed.json".into()))?;
    let rederived = jcs_canonical::derive_signed_json(manifest_yaml)?;
    if embedded_signed_json != rederived.as_slice() {
        return Err(MuragentError::SignedJsonMismatch);
    }

    // Step 6: DSSE envelope structure
    let signatures_json = archive.get_str("signatures.json")?;
    let envelope: dsse::DsseEnvelope = serde_json::from_str(signatures_json)
        .map_err(|e| MuragentError::DsseError(format!("signatures.json parse: {e}")))?;

    // Step 7: Statement structure — payload decodes to in-toto v1 Statement
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    let payload_bytes = B64
        .decode(&envelope.payload)
        .map_err(|e| MuragentError::DsseError(format!("payload base64: {e}")))?;
    let statement: InTotoStatement = serde_json::from_slice(&payload_bytes)
        .map_err(|e| MuragentError::DsseError(format!("statement parse: {e}")))?;

    if statement.type_ != "https://in-toto.io/Statement/v1" {
        return Err(MuragentError::DsseError(format!(
            "unexpected statement _type: {}",
            statement.type_
        )));
    }
    if statement.predicate_type != "https://mur.run/agent-manifest/v1" {
        return Err(MuragentError::DsseError(format!(
            "unexpected predicateType: {}",
            statement.predicate_type
        )));
    }

    let actual_manifest_sha256 = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(embedded_signed_json))
    };
    if statement.predicate.manifest_sha256 != actual_manifest_sha256 {
        return Err(MuragentError::DsseError(format!(
            "manifest_sha256 mismatch: expected {}, got {}",
            actual_manifest_sha256, statement.predicate.manifest_sha256
        )));
    }

    // Step 8: Author signature verification (verify_strict)
    dsse::verify(&envelope, "application/vnd.in-toto+json")?;

    // Step 9: Subject hashes
    verify_subjects(&statement, &archive.files_as_vec())?;

    // Step 10: Mur signature (ignored in v1)
    // Step 11: Revocation check (skipped in v1)

    let pubkey_bytes = B64
        .decode(&envelope.signatures[0].public_key)
        .map_err(|e| MuragentError::DsseError(format!("pubkey b64: {e}")))?;
    let pubkey_arr: [u8; 32] = pubkey_bytes
        .try_into()
        .map_err(|_| MuragentError::DsseError("pubkey not 32 bytes".into()))?;

    Ok(ValidationResult {
        manifest,
        author_pubkey: pubkey_arr,
        keyid: envelope.signatures[0].keyid.clone(),
    })
}
