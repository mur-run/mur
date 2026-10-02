//! `mur fleet import`: verify a `.fleet` bundle (untrusted observed data),
//! security-scan its skills, confirm, then install. Never auto-runs the fleet.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use mur_common::fleet::Fleet;
use mur_common::fleet_bundle::{
    BundleManifest, content_hash, signer_fingerprint, verify_manifest_sig,
};
use mur_common::skill::manifest::{SkillManifest, SkillScope};
use mur_common::skill::types::TrustLevel;

use super::store;

#[derive(Default)]
pub struct ImportOpts {
    pub force: bool,
    pub no_members: bool,
    pub yes: bool,
}

// I4 — gzip-bomb / unbounded decompression DoS caps.
// A fleet bundle is tiny: fleet definition + a handful of skills + a few member
// profiles. These limits leave headroom for legitimate large skill bodies while
// rejecting any explosive decompression attempt.
/// Maximum bytes allowed for any single decompressed archive entry.
const MAX_BUNDLE_ENTRY_BYTES: u64 = 8 * 1024 * 1024; // 8 MiB
/// Maximum total bytes across all decompressed archive entries.
const MAX_BUNDLE_TOTAL_BYTES: u64 = 32 * 1024 * 1024; // 32 MiB
/// Maximum number of entries (files) in a bundle archive.
const MAX_BUNDLE_ENTRIES: usize = 256;

/// Unpack the tar.gz into (manifest, path->bytes). Rejects unsafe entry paths.
pub(crate) fn unpack_bundle(bytes: &[u8]) -> Result<(BundleManifest, HashMap<String, Vec<u8>>)> {
    let gz = GzDecoder::new(bytes);
    let mut ar = tar::Archive::new(gz);
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    let mut total_bytes: u64 = 0;
    for entry in ar.entries().context("read archive")? {
        // I4: entry count cap.
        if files.len() >= MAX_BUNDLE_ENTRIES {
            bail!(
                "bundle exceeds maximum entry count ({})",
                MAX_BUNDLE_ENTRIES
            );
        }
        let mut entry = entry.context("archive entry")?;
        let path = entry
            .path()
            .context("entry path")?
            .to_string_lossy()
            .to_string();
        // Path-traversal guard: relative, no `..`, no absolute.
        if path.starts_with('/') || path.split('/').any(|c| c == "..") {
            bail!("unsafe bundle entry path: {path}");
        }
        // I4: per-entry size cap (from tar header, before reading).
        let entry_size = entry.size();
        if entry_size > MAX_BUNDLE_ENTRY_BYTES {
            bail!(
                "bundle entry '{path}' is too large ({entry_size} bytes; max {})",
                MAX_BUNDLE_ENTRY_BYTES
            );
        }
        // I4: running total cap (before reading, using header size).
        total_bytes = total_bytes.saturating_add(entry_size);
        if total_bytes > MAX_BUNDLE_TOTAL_BYTES {
            bail!(
                "bundle total uncompressed size exceeds limit ({} bytes; max {})",
                total_bytes,
                MAX_BUNDLE_TOTAL_BYTES
            );
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).context("read entry")?;
        files.insert(path, buf);
    }
    let manifest_bytes = files
        .get("bundle.yaml")
        .context("bundle.yaml missing from archive")?;
    let manifest: BundleManifest =
        serde_yaml::from_slice(manifest_bytes).context("parse bundle.yaml")?;
    Ok((manifest, files))
}

/// Member names with no local agent (`agents/<name>/profile.yaml` absent).
pub fn missing_members(mur_home: &Path, members: &[String]) -> Vec<String> {
    members
        .iter()
        .filter(|m| {
            let canon = crate::a2a_dial::canonicalize_agent_name(mur_home, m);
            !mur_home
                .join("agents")
                .join(&canon)
                .join("profile.yaml")
                .is_file()
        })
        .cloned()
        .collect()
}

/// Prompt y/N unless `yes`. Returns true to proceed.
fn confirm(prompt: &str, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    use std::io::Write;
    print!("{prompt} [y/N] ");
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .context("read stdin")?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// Official-distribution gate. Marker present ⇒ (1) bundle must be signed by
/// `publisher_fp` (a self-signed bundle claiming `distribution: official` is a
/// spoof), and (2) a matching local license (signed by the SEPARATE
/// `license_fp` key) must exist for this item + logged-in user. Bundle trust and
/// license trust use different keys so the online license key never signs bundles.
fn official_gate(
    mur_home: &Path,
    manifest: &BundleManifest,
    signer_pk: &[u8; 32],
    signature_verified: bool,
    logged_in_user: Option<&str>,
    publisher_fp: &str,
    license_fp: &str,
) -> Result<()> {
    use mur_common::muragent::dsse::keyid_from_pubkey;
    use mur_common::official::DISTRIBUTION_OFFICIAL;
    if manifest.distribution.as_deref() != Some(DISTRIBUTION_OFFICIAL) {
        return Ok(());
    }
    if !signature_verified || keyid_from_pubkey(signer_pk) != publisher_fp {
        bail!(
            "bundle claims official distribution but is not signed by the MUR official key — refusing import"
        );
    }
    let Some(user) = logged_in_user else {
        bail!(
            "this is official MUR content — log in (`mur auth login`) and get it from app.mur.run via `mur official install`"
        );
    };
    let item = format!("fleets/{}", manifest.fleet_name);
    crate::official::store::require_license_against(mur_home, &item, user, license_fp).map_err(
        |e| {
            anyhow::anyhow!(
                "{e} — official MUR content can't be shared between accounts; get it from app.mur.run via `mur official install`"
            )
        },
    )
}

pub fn cmd_fleet_import(
    mur_home: &Path,
    file: &Path,
    opts: ImportOpts,
) -> Result<(String, String, bool)> {
    // 1. Read + unpack. I4 size/count caps enforced inside unpack_bundle.
    let bytes = std::fs::read(file).with_context(|| format!("read bundle {}", file.display()))?;
    let (manifest, files) = unpack_bundle(&bytes)?;

    // 2. Verify signature (fail-closed). Unsigned → refuse unless --force.
    // C1 — `signature_verified` is true ONLY on the branch where a real signature
    // was present AND cryptographically verified; the unsigned-`--force` path
    // leaves it false so callers can gate trusted-recipe install on it (a spoofed
    // `signer_pubkey` in an unsigned bundle must never be treated as attested).
    let (_, pk) = multibase::decode(&manifest.signer_pubkey).context("decode signer pubkey")?;
    let pk: [u8; 32] = pk
        .try_into()
        .map_err(|_| anyhow::anyhow!("signer pubkey is not 32 bytes"))?;
    let signature_verified = if manifest.sig.is_none() {
        if !opts.force {
            bail!("bundle is UNSIGNED; re-run with --force to import as untrusted");
        }
        false
    } else if !verify_manifest_sig(&manifest, &pk) {
        bail!("bundle signature verification FAILED — refusing import");
    } else {
        true
    };

    // 2b. Official-distribution gate (fail-closed; see official_gate docs).
    let logged_in_user = crate::auth::load_tokens().and_then(|t| t.user_id);
    official_gate(
        mur_home,
        &manifest,
        &pk,
        signature_verified,
        logged_in_user.as_deref(),
        mur_common::skill::publisher_trust::MUR_OFFICIAL_PUBLISHER_KEY_FP,
        mur_common::skill::publisher_trust::MUR_OFFICIAL_LICENSE_KEY_FP,
    )?;

    // 3. Verify every entry's hash against the unpacked bytes (fail-closed).
    for e in &manifest.entries {
        let got = files
            .get(&e.path)
            .with_context(|| format!("bundle missing entry {}", e.path))?;
        if content_hash(got) != e.sha256 {
            bail!("hash mismatch for {} — refusing import", e.path);
        }
    }

    // 3b. Reject any archive file not declared in the signed manifest (defense-in-depth).
    let declared: std::collections::HashSet<&str> =
        manifest.entries.iter().map(|e| e.path.as_str()).collect();
    for k in files.keys() {
        if k != "bundle.yaml" && !declared.contains(k.as_str()) {
            bail!("undeclared bundle entry not covered by the signed manifest: {k}");
        }
    }

    // I3 — Recompute the signer fingerprint from the verified pubkey and display
    // ONLY the derived value; reject if the manifest's stored value mismatches.
    // An empty signer_fingerprint is also rejected: export.rs always populates it,
    // so a missing value indicates a crafted or stripped bundle.
    let derived_fp = signer_fingerprint(&manifest.signer_pubkey);
    if manifest.signer_fingerprint != derived_fp {
        bail!("manifest signer_fingerprint does not match signer_pubkey — refusing import");
    }

    // 4. Provenance + plan (two-tier trust: Phase A pins an empty official set, so
    //    every bundle is a peer/TOFU import → lowest trust, scan + confirm).
    let skill_paths: Vec<&String> = manifest
        .entries
        .iter()
        .map(|e| &e.path)
        .filter(|p| p.starts_with("skills/"))
        .collect();
    // I3: Use derived_fp (computed from the verified pubkey), never manifest.signer_fingerprint.
    println!(
        "Fleet bundle '{}' from signer {}",
        manifest.fleet_name, derived_fp
    );
    println!(
        "  signature: {}",
        if manifest.sig.is_some() {
            "verified"
        } else {
            "UNSIGNED (--force)"
        }
    );
    println!("  skills: {}", skill_paths.len());
    println!("  members declared: {}", manifest.members.join(", "));

    // 5. Security-scan each bundled skill; surface findings.
    // C1 — Validate skill name before deriving install path.
    let mut parsed_skills: Vec<(String, SkillManifest)> = Vec::new();
    for path in &skill_paths {
        let m: SkillManifest = serde_yaml::from_slice(
            files
                .get(*path)
                .with_context(|| format!("bundle missing entry {path}"))?,
        )
        .with_context(|| format!("parse {path}"))?;

        // C1a: validate the internal `name` field is a safe identifier.
        if !mur_common::skill::is_valid_skill_name(&m.name) {
            bail!(
                "skill at '{}' has an invalid name field '{}' — refusing import",
                path,
                m.name
            );
        }
        // C1b: assert `name` matches the archive path's directory component
        // (i.e. the entry must be exactly `skills/<name>/skill.yaml`).
        let expected_path = format!("skills/{}/skill.yaml", m.name);
        if *path != &expected_path {
            bail!(
                "skill name mismatch: entry '{}' has internal name '{}' (expected path '{}')",
                path,
                m.name,
                expected_path
            );
        }

        let report = mur_common::skill::scan::scan_skill(&m)
            .map_err(|e| anyhow::anyhow!("scan {path}: {e}"))?;
        if report.has_blocking_findings() {
            println!("  ⚠ security findings in {}:", m.name);
            for line in report.human_summary() {
                println!("      {line}");
            }
        }
        parsed_skills.push((m.name.clone(), m));
    }

    // 6. Fleet name-conflict check (fail-fast before prompting the user).
    if store::fleet_path(mur_home, &manifest.fleet_name).is_file() && !opts.force {
        bail!(
            "fleet '{}' already exists — re-run with --force to overwrite",
            manifest.fleet_name
        );
    }

    // 7. HITL confirm — nothing written before approval.
    if !confirm("Install this fleet + skills?", opts.yes)? {
        bail!("import cancelled");
    }

    // 8. Install skills: scope:Fleet preserved, trust DOWNGRADED to Sandboxed.
    for (name, mut m) in parsed_skills {
        let dir = mur_common::skill::store::global_skill_dir(mur_home, &name);
        if dir.join("skill.yaml").is_file() && !opts.force {
            println!("  skill '{name}' exists — skipping (use --force to overwrite)");
            continue;
        }
        // enforce scope:Fleet for this fleet (provenance ≠ claim)
        m.scope = SkillScope::Fleet;
        m.fleet = Some(manifest.fleet_name.clone());
        m.project = None;
        mur_common::skill::store::write_to_dir(&dir, &m)
            .map_err(|e| anyhow::anyhow!("install skill {name}: {e}"))?;

        // I5 — Explicitly register the imported skill in the trust store at
        // Sandboxed so the entry exists (set_trust_level mutates; without an
        // entry to mutate it is a no-op). Mirror the pattern from skill_install.rs.
        let trust_key = mur_common::skill::content_hash_for_trust(&m)
            .map_err(|e| anyhow::anyhow!("hash skill {name}: {e}"))?;
        let mut trust = mur_common::trust::skills::SkillTrustStore::load(mur_home)
            .map_err(|e| anyhow::anyhow!("load trust: {e}"))?;
        trust.insert(
            trust_key,
            mur_common::trust::skills::TrustEntry {
                name: name.clone(),
                version: m.version.clone(),
                level: TrustLevel::Sandboxed,
                installed_at: chrono::Utc::now().to_rfc3339(),
                publisher: Some(m.publisher.clone()),
                ..Default::default()
            },
        );
        trust
            .save(mur_home)
            .map_err(|e| anyhow::anyhow!("save trust: {e}"))?;
    }

    // 9. Install the fleet definition.
    // C2a — Validate fleet name and cross-check against signed manifest.
    let fleet: Fleet = serde_yaml::from_slice(
        files
            .get("fleet.yaml")
            .context("bundle missing fleet.yaml")?,
    )
    .context("parse fleet.yaml")?;

    if !mur_common::fleet::valid_fleet_name(&fleet.name) {
        bail!(
            "bundle fleet.yaml has an invalid fleet name '{}'",
            fleet.name
        );
    }
    if fleet.name != manifest.fleet_name {
        bail!(
            "bundle fleet name mismatch: manifest '{}' vs fleet.yaml '{}'",
            manifest.fleet_name,
            fleet.name
        );
    }
    // C2a — members in fleet.yaml must match the signed manifest.members exactly.
    if fleet.members != manifest.members {
        bail!(
            "bundle members mismatch: manifest {:?} vs fleet.yaml {:?}",
            manifest.members,
            fleet.members
        );
    }
    // C2a — channel_id must be canonical so the loop (reads fleet.channel_id),
    // the commander directive path, and the daemon (both reconstruct
    // fleet-<name>) all govern the SAME channel. A non-canonical id smuggled in
    // an untrusted bundle would otherwise escape commander kills on a manual run.
    let canonical_channel_id = format!("fleet-{}", fleet.name);
    if fleet.channel_id != canonical_channel_id {
        bail!(
            "bundle fleet.yaml channel_id '{}' is not canonical (expected '{}') — refusing import",
            fleet.channel_id,
            canonical_channel_id
        );
    }

    store::save_fleet(mur_home, &fleet)?;

    // 10. Members: install bundled (Task 4) or report missing. Never auto-run.
    if manifest.includes_members && !opts.no_members {
        install_bundled_members(mur_home, &manifest, &files, opts.force, opts.yes)?;
    }
    let missing = missing_members(mur_home, &fleet.members);
    if missing.is_empty() {
        println!("Imported fleet '{}'. All members present.", fleet.name);
    } else {
        println!(
            "Imported fleet '{}'. Missing members: {} — create them or import a --with-members bundle before running.",
            fleet.name,
            missing.join(", ")
        );
    }

    // Best-effort program-deps preflight — informational only, never fails
    // the import (the fleet + skills are already installed above).
    let _ = (|| -> Result<()> {
        let deps = crate::cmd::deps::aggregate_fleet(mur_home, &fleet.name)?;
        let report = crate::cmd::deps::doctor::build_report(&deps, mur_home);
        crate::cmd::deps::doctor::print_report(
            &report,
            &format!("mur fleet install-deps {}", fleet.name),
        );
        if crate::cmd::deps::doctor::missing_count(&report) > 0 {
            println!(
                "Run `mur fleet install-deps {}` to install them.",
                fleet.name
            );
        }
        Ok(())
    })();

    Ok((
        manifest.fleet_name.clone(),
        derived_fp.clone(),
        signature_verified,
    ))
}

/// Install each bundled member profile. Skips members that already exist locally
/// (never overwrites). Generates a FRESH local identity for new members.
pub(crate) fn install_bundled_members(
    mur_home: &Path,
    manifest: &BundleManifest,
    files: &HashMap<String, Vec<u8>>,
    _force: bool,
    yes: bool,
) -> Result<()> {
    use mur_common::agent::AgentProfile;
    use mur_common::identity::AgentIdentity;

    for member in &manifest.members {
        let canon = crate::a2a_dial::canonicalize_agent_name(mur_home, member);
        let dir = mur_home.join("agents").join(&canon);
        let key = format!("members/{canon}/profile.yaml");
        let Some(profile_bytes) = files.get(&key) else {
            // Bundler skipped this member (not present on exporter) — nothing to install.
            continue;
        };
        if dir.join("profile.yaml").is_file() {
            println!("  member '{canon}' already exists — skipping");
            continue;
        }
        // Show entitlements before asking.
        let profile_str = String::from_utf8_lossy(profile_bytes);
        let ent_line = profile_str
            .lines()
            .find(|l| l.trim_start().starts_with("entitlements"))
            .unwrap_or("entitlements: (none)");
        println!("  member '{canon}': {ent_line}");
        if !confirm(&format!("Install agent '{canon}'?"), yes)? {
            println!("  skipping '{canon}'");
            continue;
        }
        std::fs::create_dir_all(&dir).with_context(|| format!("create agent dir for '{canon}'"))?;

        // I6 — Generate a FRESH local identity first, then rewrite the profile's
        // `identity:` block so the advertised pubkey matches the fresh local key.
        // Never copy the exporter's pubkey/key_version into the installed profile.
        let fresh_id = AgentIdentity::generate();
        fresh_id
            .save(&dir)
            .with_context(|| format!("generate identity for '{canon}'"))?;

        // Parse the bundled profile and overwrite the identity block with the
        // fresh public key so profile.yaml is consistent with identity.pub.
        // Fail-closed: if the bundled profile does not parse as AgentProfile
        // (missing required fields, wrong schema, or tampered data), bail.
        // A member created by `mur agent create` always produces a valid profile;
        // an unparseable one is suspicious and must not be installed.
        match serde_yaml::from_slice::<AgentProfile>(profile_bytes) {
            Ok(mut profile) => {
                profile.identity.pubkey = fresh_id.public_key_multibase();
                profile.identity.key_version = 0;
                profile.identity.previous_pubkey = None;
                profile.identity.previous_key_version = None;
                profile.identity.grace_expires_at = None;
                let profile_yaml = serde_yaml::to_string(&profile)
                    .with_context(|| format!("serialize profile for '{canon}'"))?;
                std::fs::write(dir.join("profile.yaml"), profile_yaml.as_bytes())
                    .with_context(|| format!("write profile for '{canon}'"))?;
                // #712: the imported member's entitlements are what the user
                // just approved by importing; pin them over any stale pin.
                mur_common::entitlements_pin::write_pin(mur_home, &canon, &profile.entitlements)
                    .with_context(|| format!("pin entitlements for '{canon}'"))?;
            }
            Err(e) => {
                bail!(
                    "member '{canon}' profile.yaml is malformed/unsupported — refusing to install member: {e}"
                );
            }
        }

        println!("  installed member '{canon}' with fresh identity");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
