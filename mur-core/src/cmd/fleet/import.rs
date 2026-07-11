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

pub fn cmd_fleet_import(
    mur_home: &Path,
    file: &Path,
    opts: ImportOpts,
) -> Result<(String, String)> {
    // 1. Read + unpack. I4 size/count caps enforced inside unpack_bundle.
    let bytes = std::fs::read(file).with_context(|| format!("read bundle {}", file.display()))?;
    let (manifest, files) = unpack_bundle(&bytes)?;

    // 2. Verify signature (fail-closed). Unsigned → refuse unless --force.
    let (_, pk) = multibase::decode(&manifest.signer_pubkey).context("decode signer pubkey")?;
    let pk: [u8; 32] = pk
        .try_into()
        .map_err(|_| anyhow::anyhow!("signer pubkey is not 32 bytes"))?;
    if manifest.sig.is_none() {
        if !opts.force {
            bail!("bundle is UNSIGNED; re-run with --force to import as untrusted");
        }
    } else if !verify_manifest_sig(&manifest, &pk) {
        bail!("bundle signature verification FAILED — refusing import");
    }

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

    Ok((manifest.fleet_name.clone(), derived_fp.clone()))
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
mod tests {
    use super::*;
    use mur_common::fleet::Fleet;
    use mur_common::identity::AgentIdentity;
    use mur_common::skill::manifest::SkillScope;
    use mur_common::skill::types::TrustLevel;

    fn export_fixture(home: &std::path::Path) -> std::path::PathBuf {
        // concierge + a fleet-scoped skill + a fleet, then export
        let dir = home.join("agents").join("mur");
        std::fs::create_dir_all(&dir).unwrap();
        AgentIdentity::generate().save(&dir).unwrap();
        let m: mur_common::skill::manifest::SkillManifest = serde_yaml::from_str(
            "name: triage\nversion: 1.0.0\npublisher: human:t\ndescription: t\n\
             category: context\nscope: fleet\nfleet: dev\ncontent:\n  abstract: a\n  context: body\n",
        )
        .unwrap();
        mur_common::skill::store::write_to_dir(
            &mur_common::skill::store::global_skill_dir(home, "triage"),
            &m,
        )
        .unwrap();
        let fleet = Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "ship".into(),
            router: None,
            members: vec!["pm".into(), "qa".into()],
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
            team_id: None,
            parallel: None,
            requires_programs: vec![],
        };
        crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
        let out = home.join("dev.fleet");
        crate::cmd::fleet::export::cmd_fleet_export(
            home,
            "dev",
            false,
            Some(out.clone()),
            "2026-06-20T00:00:00Z",
        )
        .unwrap();
        out
    }

    #[test]
    fn import_roundtrip_installs_fleet_and_skill_at_low_trust() {
        let src = tempfile::tempdir().unwrap();
        let bundle = export_fixture(src.path());

        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        cmd_fleet_import(
            home,
            &bundle,
            ImportOpts {
                force: false,
                no_members: false,
                yes: true,
            },
        )
        .unwrap();

        // fleet installed
        let f = crate::cmd::fleet::store::load_fleet(home, "dev").unwrap();
        assert_eq!(f.members, vec!["pm".to_string(), "qa".to_string()]);
        // skill installed, scope:Fleet preserved, trust downgraded to Sandboxed
        let m = mur_common::skill::local::load_installed(home, "triage").unwrap();
        assert_eq!(m.scope, SkillScope::Fleet);
        assert_eq!(m.fleet.as_deref(), Some("dev"));
        assert_eq!(
            mur_common::skill::local::get_trust_level(home, "triage").unwrap(),
            TrustLevel::Sandboxed
        );
    }

    #[test]
    fn import_refuses_tampered_bundle() {
        let src = tempfile::tempdir().unwrap();
        let bundle = export_fixture(src.path());
        let mut bytes = std::fs::read(&bundle).unwrap();
        let n = bytes.len();
        bytes[n / 2] ^= 0xFF; // corrupt the archive
        let bad = src.path().join("bad.fleet");
        std::fs::write(&bad, &bytes).unwrap();

        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        let err = cmd_fleet_import(
            home,
            &bad,
            ImportOpts {
                force: false,
                no_members: false,
                yes: true,
            },
        )
        .unwrap_err();
        // fail-closed: refused AND nothing installed (no partial write on tamper).
        assert!(
            !crate::cmd::fleet::store::fleet_path(home, "dev").is_file(),
            "tampered bundle must not install the fleet; err={err:#}"
        );
        assert!(
            !mur_common::skill::store::global_skill_dir(home, "triage")
                .join("skill.yaml")
                .is_file(),
            "tampered bundle must not install skills; err={err:#}"
        );
    }

    #[test]
    fn import_refuses_name_conflict_without_force() {
        let src = tempfile::tempdir().unwrap();
        let bundle = export_fixture(src.path());
        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        let opts = || ImportOpts {
            force: false,
            no_members: false,
            yes: true,
        };
        cmd_fleet_import(home, &bundle, opts()).unwrap();
        let err = cmd_fleet_import(home, &bundle, opts()).unwrap_err();
        assert!(format!("{err:#}").contains("exists"));
        // with force it succeeds
        cmd_fleet_import(
            home,
            &bundle,
            ImportOpts {
                force: true,
                no_members: false,
                yes: true,
            },
        )
        .unwrap();
    }

    #[test]
    fn missing_members_reports_absent_agents() {
        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        // create agent "pm" locally
        let pm = home.join("agents").join("pm");
        std::fs::create_dir_all(&pm).unwrap();
        std::fs::write(pm.join("profile.yaml"), "name: pm\n").unwrap();
        let missing = missing_members(home, &["pm".into(), "qa".into()]);
        assert_eq!(missing, vec!["qa".to_string()]);
    }

    #[test]
    fn import_with_members_installs_fresh_identity() {
        // Setup: source with a concierge, a pm agent, and a fleet.
        let src = tempfile::tempdir().unwrap();
        let s = src.path();
        let mur_dir = s.join("agents").join("mur");
        std::fs::create_dir_all(&mur_dir).unwrap();
        mur_common::identity::AgentIdentity::generate()
            .save(&mur_dir)
            .unwrap();
        let pm_dir = s.join("agents").join("pm");
        std::fs::create_dir_all(&pm_dir).unwrap();
        // Must be a valid AgentProfile — write a parseable profile with the right name.
        let mut pm_profile = mur_common::agent::AgentProfile::default_for_tests();
        pm_profile.name = "pm".into();
        let pm_profile_yaml = serde_yaml::to_string(&pm_profile).unwrap();
        std::fs::write(pm_dir.join("profile.yaml"), pm_profile_yaml.as_bytes()).unwrap();
        mur_common::identity::AgentIdentity::generate()
            .save(&pm_dir)
            .unwrap();
        let fleet = Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "g".into(),
            router: None,
            members: vec!["pm".into()],
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
            team_id: None,
            parallel: None,
            requires_programs: vec![],
        };
        crate::cmd::fleet::store::save_fleet(s, &fleet).unwrap();
        let bundle = s.join("dev.fleet");
        crate::cmd::fleet::export::cmd_fleet_export(
            s,
            "dev",
            true,
            Some(bundle.clone()),
            "2026-06-20T00:00:00Z",
        )
        .unwrap();

        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        cmd_fleet_import(
            home,
            &bundle,
            ImportOpts {
                force: false,
                no_members: false,
                yes: true,
            },
        )
        .unwrap();

        let pm2 = home.join("agents").join("pm");
        assert!(pm2.join("profile.yaml").is_file());
        // fresh identity generated — key must NOT be the same bytes as the source
        let src_key = std::fs::read(pm_dir.join("identity.key")).unwrap();
        let dst_key = std::fs::read(pm2.join("identity.key")).unwrap();
        assert_ne!(
            src_key, dst_key,
            "import must regenerate identity, not copy the private key"
        );
    }

    #[test]
    fn import_with_members_never_overwrites_existing_agent() {
        let src = tempfile::tempdir().unwrap();
        let s = src.path();
        let mur_dir = s.join("agents").join("mur");
        std::fs::create_dir_all(&mur_dir).unwrap();
        mur_common::identity::AgentIdentity::generate()
            .save(&mur_dir)
            .unwrap();
        let pm_dir = s.join("agents").join("pm");
        std::fs::create_dir_all(&pm_dir).unwrap();
        // Must be a valid AgentProfile so the export round-trip succeeds.
        let mut pm_profile = mur_common::agent::AgentProfile::default_for_tests();
        pm_profile.name = "pm".into();
        let pm_profile_yaml = serde_yaml::to_string(&pm_profile).unwrap();
        std::fs::write(pm_dir.join("profile.yaml"), pm_profile_yaml.as_bytes()).unwrap();
        mur_common::identity::AgentIdentity::generate()
            .save(&pm_dir)
            .unwrap();
        let fleet = Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "g".into(),
            router: None,
            members: vec!["pm".into()],
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
            team_id: None,
            parallel: None,
            requires_programs: vec![],
        };
        crate::cmd::fleet::store::save_fleet(s, &fleet).unwrap();
        let bundle = s.join("dev.fleet");
        crate::cmd::fleet::export::cmd_fleet_export(
            s,
            "dev",
            true,
            Some(bundle.clone()),
            "2026-06-20T00:00:00Z",
        )
        .unwrap();

        // Pre-create pm on the destination
        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        let existing_pm = home.join("agents").join("pm");
        std::fs::create_dir_all(&existing_pm).unwrap();
        std::fs::write(
            existing_pm.join("profile.yaml"),
            "name: pm\nexisting: true\n",
        )
        .unwrap();
        let original_key = {
            let id = mur_common::identity::AgentIdentity::generate();
            id.save(&existing_pm).unwrap();
            std::fs::read(existing_pm.join("identity.key")).unwrap()
        };

        cmd_fleet_import(
            home,
            &bundle,
            ImportOpts {
                force: false,
                no_members: false,
                yes: true,
            },
        )
        .unwrap();

        // profile must NOT be overwritten
        let kept = std::fs::read_to_string(existing_pm.join("profile.yaml")).unwrap();
        assert!(
            kept.contains("existing: true"),
            "existing agent must not be overwritten"
        );
        // identity key must NOT be overwritten
        let key_after = std::fs::read(existing_pm.join("identity.key")).unwrap();
        assert_eq!(
            original_key, key_after,
            "existing identity must not be overwritten"
        );
    }

    // ── Security regression tests ──────────────────────────────────────────────

    /// C1 — Skill name path-traversal: a signed bundle whose `skills/foo/skill.yaml`
    /// bytes carry `name: ../../evil` must be REFUSED, and nothing written outside
    /// `<mur_home>/skills`.
    #[test]
    fn import_refuses_skill_name_path_traversal() {
        use mur_common::fleet_bundle::{
            BundleEntry, BundleManifest, FLEET_BUNDLE_FORMAT, content_hash, manifest_sign_input,
            signer_fingerprint,
        };
        use mur_common::identity::AgentIdentity;

        let src = tempfile::tempdir().unwrap();
        let s = src.path();

        // Build a valid concierge identity for signing.
        let id_dir = s.join("agents").join("mur");
        std::fs::create_dir_all(&id_dir).unwrap();
        let id = AgentIdentity::generate();
        id.save(&id_dir).unwrap();
        let signer_pubkey = id.public_key_multibase();

        // Craft a fleet.yaml and a skill YAML whose `name` field contains a path traversal.
        let fleet_yaml = "name: dev\ndisplay_name: \"\"\ngoal: g\nrouter: ~\nmembers: []\nchannel_id: fleet-dev\nrules: []\nskills: []\nloop_cfg: ~\n";
        // The archive path is `skills/foo/skill.yaml` but the internal name is `../../evil`.
        let evil_skill_yaml = "name: ../../evil\nversion: 1.0.0\npublisher: human:t\n\
            description: bad\ncategory: context\ncontent:\n  abstract: a\n  context: body\n";

        let files: Vec<(String, Vec<u8>)> = vec![
            ("fleet.yaml".into(), fleet_yaml.as_bytes().to_vec()),
            (
                "skills/foo/skill.yaml".into(),
                evil_skill_yaml.as_bytes().to_vec(),
            ),
        ];

        let entries: Vec<BundleEntry> = files
            .iter()
            .map(|(p, b)| BundleEntry {
                path: p.clone(),
                sha256: content_hash(b),
            })
            .collect();

        let mut manifest = BundleManifest {
            format_version: FLEET_BUNDLE_FORMAT,
            fleet_name: "dev".into(),
            created_at: "2026-06-20T00:00:00Z".into(),
            signer_fingerprint: signer_fingerprint(&signer_pubkey),
            signer_pubkey,
            includes_members: false,
            members: vec![],
            entries,
            sig: None,
        };
        let input = manifest_sign_input(&manifest);
        manifest.sig = Some(multibase::encode(
            multibase::Base::Base58Btc,
            id.sign_bytes(&input),
        ));

        // Build tar.gz bundle.
        let bundle_bytes = build_evil_bundle(&manifest, &files);
        let bundle_path = s.join("evil.fleet");
        std::fs::write(&bundle_path, &bundle_bytes).unwrap();

        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        let err = cmd_fleet_import(
            home,
            &bundle_path,
            ImportOpts {
                force: false,
                no_members: false,
                yes: true,
            },
        )
        .unwrap_err();

        let msg = format!("{err:#}");
        assert!(
            msg.contains("invalid name") || msg.contains("mismatch"),
            "expected path-traversal refusal, got: {msg}"
        );
        // Nothing written outside skills/
        assert!(
            !home.join("..").join("evil").exists(),
            "path traversal must not escape mur_home"
        );
        assert!(
            !home
                .join("skills")
                .join("../../evil")
                .join("skill.yaml")
                .exists(),
            "traversal skill must not be installed"
        );
    }

    /// C2 — Fleet name path-traversal: a bundle whose `fleet.yaml` `name` differs
    /// from `manifest.fleet_name` must be REFUSED with nothing written.
    #[test]
    fn import_refuses_fleet_name_mismatch() {
        use mur_common::fleet_bundle::{
            BundleEntry, BundleManifest, FLEET_BUNDLE_FORMAT, content_hash, manifest_sign_input,
            signer_fingerprint,
        };
        use mur_common::identity::AgentIdentity;

        let src = tempfile::tempdir().unwrap();
        let s = src.path();
        let id_dir = s.join("agents").join("mur");
        std::fs::create_dir_all(&id_dir).unwrap();
        let id = AgentIdentity::generate();
        id.save(&id_dir).unwrap();
        let signer_pubkey = id.public_key_multibase();

        // fleet.yaml says `name: ../../evil` but manifest.fleet_name says `dev`.
        let fleet_yaml = "name: ../../evil\ndisplay_name: \"\"\ngoal: g\nrouter: ~\nmembers: []\nchannel_id: fleet-dev\nrules: []\nskills: []\nloop_cfg: ~\n";
        let files: Vec<(String, Vec<u8>)> =
            vec![("fleet.yaml".into(), fleet_yaml.as_bytes().to_vec())];
        let entries: Vec<BundleEntry> = files
            .iter()
            .map(|(p, b)| BundleEntry {
                path: p.clone(),
                sha256: content_hash(b),
            })
            .collect();
        let mut manifest = BundleManifest {
            format_version: FLEET_BUNDLE_FORMAT,
            fleet_name: "dev".into(), // manifest says dev, fleet.yaml says ../../evil
            created_at: "2026-06-20T00:00:00Z".into(),
            signer_fingerprint: signer_fingerprint(&signer_pubkey),
            signer_pubkey,
            includes_members: false,
            members: vec![],
            entries,
            sig: None,
        };
        let input = manifest_sign_input(&manifest);
        manifest.sig = Some(multibase::encode(
            multibase::Base::Base58Btc,
            id.sign_bytes(&input),
        ));
        let bundle_bytes = build_evil_bundle(&manifest, &files);
        let bundle_path = s.join("mismatch.fleet");
        std::fs::write(&bundle_path, &bundle_bytes).unwrap();

        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        let err = cmd_fleet_import(
            home,
            &bundle_path,
            ImportOpts {
                force: false,
                no_members: false,
                yes: true,
            },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("mismatch") || msg.contains("invalid fleet name"),
            "expected fleet name refusal, got: {msg}"
        );
        // Nothing written
        assert!(
            !home.join("fleets").exists()
                || home
                    .join("fleets")
                    .read_dir()
                    .map(|mut d| d.next().is_none())
                    .unwrap_or(true),
            "fleet must not be written on name mismatch"
        );
    }

    /// Governance bypass guard: a correctly-signed bundle whose fleet.yaml
    /// `channel_id` is non-canonical (≠ `fleet-<name>`) must be refused — else the
    /// loop would govern a different channel than the commander/daemon write to.
    #[test]
    fn import_refuses_noncanonical_channel_id() {
        use mur_common::fleet_bundle::{
            BundleEntry, BundleManifest, FLEET_BUNDLE_FORMAT, content_hash, manifest_sign_input,
            signer_fingerprint,
        };
        use mur_common::identity::AgentIdentity;

        let src = tempfile::tempdir().unwrap();
        let s = src.path();
        let id_dir = s.join("agents").join("mur");
        std::fs::create_dir_all(&id_dir).unwrap();
        let id = AgentIdentity::generate();
        id.save(&id_dir).unwrap();
        let signer_pubkey = id.public_key_multibase();

        // name `dev` (matches manifest, valid) but channel_id smuggles `fleet-evil`.
        let fleet_yaml = "name: dev\ndisplay_name: \"\"\ngoal: g\nrouter: ~\nmembers: []\nchannel_id: fleet-evil\nrules: []\nskills: []\nloop_cfg: ~\n";
        let files: Vec<(String, Vec<u8>)> =
            vec![("fleet.yaml".into(), fleet_yaml.as_bytes().to_vec())];
        let entries: Vec<BundleEntry> = files
            .iter()
            .map(|(p, b)| BundleEntry {
                path: p.clone(),
                sha256: content_hash(b),
            })
            .collect();
        let mut manifest = BundleManifest {
            format_version: FLEET_BUNDLE_FORMAT,
            fleet_name: "dev".into(),
            created_at: "2026-06-20T00:00:00Z".into(),
            signer_fingerprint: signer_fingerprint(&signer_pubkey),
            signer_pubkey,
            includes_members: false,
            members: vec![],
            entries,
            sig: None,
        };
        let input = manifest_sign_input(&manifest);
        manifest.sig = Some(multibase::encode(
            multibase::Base::Base58Btc,
            id.sign_bytes(&input),
        ));
        let bundle_bytes = build_evil_bundle(&manifest, &files);
        let bundle_path = s.join("noncanon.fleet");
        std::fs::write(&bundle_path, &bundle_bytes).unwrap();

        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        let err = cmd_fleet_import(
            home,
            &bundle_path,
            ImportOpts {
                force: false,
                no_members: false,
                yes: true,
            },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("not canonical"),
            "expected canonical-channel refusal, got: {msg}"
        );
        // Nothing written
        assert!(
            !home.join("fleets").exists()
                || home
                    .join("fleets")
                    .read_dir()
                    .map(|mut d| d.next().is_none())
                    .unwrap_or(true),
            "fleet must not be written on non-canonical channel_id"
        );
    }

    /// I3 — Spoofable fingerprint: a correctly-signed manifest whose stored
    /// `signer_fingerprint` ≠ `signer_fingerprint(signer_pubkey)` must be refused.
    #[test]
    fn import_refuses_mismatched_signer_fingerprint() {
        use mur_common::fleet_bundle::{
            BundleEntry, BundleManifest, FLEET_BUNDLE_FORMAT, content_hash, manifest_sign_input,
        };
        use mur_common::identity::AgentIdentity;

        let src = tempfile::tempdir().unwrap();
        let s = src.path();
        let id_dir = s.join("agents").join("mur");
        std::fs::create_dir_all(&id_dir).unwrap();
        let id = AgentIdentity::generate();
        id.save(&id_dir).unwrap();
        let signer_pubkey = id.public_key_multibase();

        let fleet_yaml = "name: dev\ndisplay_name: \"\"\ngoal: g\nrouter: ~\nmembers: []\nchannel_id: fleet-dev\nrules: []\nskills: []\nloop_cfg: ~\n";
        let files: Vec<(String, Vec<u8>)> =
            vec![("fleet.yaml".into(), fleet_yaml.as_bytes().to_vec())];
        let entries: Vec<BundleEntry> = files
            .iter()
            .map(|(p, b)| BundleEntry {
                path: p.clone(),
                sha256: content_hash(b),
            })
            .collect();

        let mut manifest = BundleManifest {
            format_version: FLEET_BUNDLE_FORMAT,
            fleet_name: "dev".into(),
            created_at: "2026-06-20T00:00:00Z".into(),
            // Deliberately wrong fingerprint — attacker-controlled, not derived from pubkey.
            signer_fingerprint: "dead-beef".into(),
            signer_pubkey,
            includes_members: false,
            members: vec![],
            entries,
            sig: None,
        };
        // Sign correctly so the sig check passes — the fingerprint mismatch must still fire.
        let input = manifest_sign_input(&manifest);
        manifest.sig = Some(multibase::encode(
            multibase::Base::Base58Btc,
            id.sign_bytes(&input),
        ));
        let bundle_bytes = build_evil_bundle(&manifest, &files);
        let bundle_path = s.join("badfp.fleet");
        std::fs::write(&bundle_path, &bundle_bytes).unwrap();

        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        let err = cmd_fleet_import(
            home,
            &bundle_path,
            ImportOpts {
                force: false,
                no_members: false,
                yes: true,
            },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("signer_fingerprint"),
            "expected fingerprint mismatch refusal, got: {msg}"
        );
    }

    /// I3 regression — empty signer_fingerprint bypass: a correctly-signed manifest
    /// whose `signer_fingerprint` is empty string must be refused (the derived
    /// fingerprint is never empty, so they will never match).
    #[test]
    fn import_refuses_empty_signer_fingerprint() {
        use mur_common::fleet_bundle::{
            BundleEntry, BundleManifest, FLEET_BUNDLE_FORMAT, content_hash, manifest_sign_input,
        };
        use mur_common::identity::AgentIdentity;

        let src = tempfile::tempdir().unwrap();
        let s = src.path();
        let id_dir = s.join("agents").join("mur");
        std::fs::create_dir_all(&id_dir).unwrap();
        let id = AgentIdentity::generate();
        id.save(&id_dir).unwrap();
        let signer_pubkey = id.public_key_multibase();

        let fleet_yaml = "name: dev\ndisplay_name: \"\"\ngoal: g\nrouter: ~\nmembers: []\nchannel_id: fleet-dev\nrules: []\nskills: []\nloop_cfg: ~\n";
        let files: Vec<(String, Vec<u8>)> =
            vec![("fleet.yaml".into(), fleet_yaml.as_bytes().to_vec())];
        let entries: Vec<BundleEntry> = files
            .iter()
            .map(|(p, b)| BundleEntry {
                path: p.clone(),
                sha256: content_hash(b),
            })
            .collect();

        let mut manifest = BundleManifest {
            format_version: FLEET_BUNDLE_FORMAT,
            fleet_name: "dev".into(),
            created_at: "2026-06-20T00:00:00Z".into(),
            // Empty fingerprint — the old guard would skip the check; the new guard rejects it.
            signer_fingerprint: String::new(),
            signer_pubkey,
            includes_members: false,
            members: vec![],
            entries,
            sig: None,
        };
        // Sign correctly so the sig check passes — the empty fingerprint must still be rejected.
        let input = manifest_sign_input(&manifest);
        manifest.sig = Some(multibase::encode(
            multibase::Base::Base58Btc,
            id.sign_bytes(&input),
        ));
        let bundle_bytes = build_evil_bundle(&manifest, &files);
        let bundle_path = s.join("emptyfp.fleet");
        std::fs::write(&bundle_path, &bundle_bytes).unwrap();

        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        let err = cmd_fleet_import(
            home,
            &bundle_path,
            ImportOpts {
                force: false,
                no_members: false,
                yes: true,
            },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("signer_fingerprint"),
            "expected empty-fingerprint refusal, got: {msg}"
        );
    }

    /// I4 — gzip-bomb: a bundle with an entry exceeding the per-entry cap is refused.
    #[test]
    fn import_refuses_oversized_bundle_entry() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use mur_common::fleet_bundle::{
            BundleEntry, BundleManifest, FLEET_BUNDLE_FORMAT, content_hash, manifest_sign_input,
            signer_fingerprint,
        };
        use mur_common::identity::AgentIdentity;

        let src = tempfile::tempdir().unwrap();
        let s = src.path();
        let id_dir = s.join("agents").join("mur");
        std::fs::create_dir_all(&id_dir).unwrap();
        let id = AgentIdentity::generate();
        id.save(&id_dir).unwrap();
        let signer_pubkey = id.public_key_multibase();

        // Craft an entry that exceeds MAX_BUNDLE_ENTRY_BYTES (8 MiB) when decompressed.
        // We use a large repeated zero byte sequence (highly compressible = gzip-bomb pattern).
        let oversized: Vec<u8> = vec![0u8; (MAX_BUNDLE_ENTRY_BYTES + 1) as usize];
        let fleet_yaml = "name: dev\ndisplay_name: \"\"\ngoal: g\nrouter: ~\nmembers: []\nchannel_id: fleet-dev\nrules: []\nskills: []\nloop_cfg: ~\n".as_bytes();

        // We sign a manifest for this large entry so the sig check passes first.
        // The DoS cap must fire during unpack_bundle (before sig check or after — doesn't matter
        // as long as it fires).
        let files_data: Vec<(&str, &[u8])> =
            vec![("fleet.yaml", fleet_yaml), ("big.bin", &oversized)];
        let entries: Vec<BundleEntry> = files_data
            .iter()
            .map(|(p, b)| BundleEntry {
                path: p.to_string(),
                sha256: content_hash(b),
            })
            .collect();
        let mut manifest = BundleManifest {
            format_version: FLEET_BUNDLE_FORMAT,
            fleet_name: "dev".into(),
            created_at: "2026-06-20T00:00:00Z".into(),
            signer_fingerprint: signer_fingerprint(&signer_pubkey),
            signer_pubkey,
            includes_members: false,
            members: vec![],
            entries,
            sig: None,
        };
        let input = manifest_sign_input(&manifest);
        manifest.sig = Some(multibase::encode(
            multibase::Base::Base58Btc,
            id.sign_bytes(&input),
        ));

        // Build the tar.gz with the oversized entry.
        let mut buf = Vec::new();
        {
            let gz = GzEncoder::new(&mut buf, Compression::default());
            let mut tar = tar::Builder::new(gz);
            let manifest_yaml = serde_yaml::to_string(&manifest).unwrap();
            let add = |tar: &mut tar::Builder<_>, path: &str, data: &[u8]| {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                tar.append_data(&mut h, path, data).unwrap();
            };
            add(&mut tar, "bundle.yaml", manifest_yaml.as_bytes());
            add(&mut tar, "fleet.yaml", fleet_yaml);
            add(&mut tar, "big.bin", &oversized);
            tar.into_inner().unwrap().finish().unwrap();
        }

        let bundle_path = src.path().join("bomb.fleet");
        std::fs::write(&bundle_path, &buf).unwrap();

        let dst = tempfile::tempdir().unwrap();
        let err = cmd_fleet_import(
            dst.path(),
            &bundle_path,
            ImportOpts {
                force: false,
                no_members: false,
                yes: true,
            },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("too large") || msg.contains("exceeds"),
            "expected DoS cap refusal, got: {msg}"
        );
    }

    /// I5 — Trust store: after import, the skill must have an *explicit* Sandboxed
    /// entry in the trust store (not merely rely on the default). Also verifies a
    /// skill claiming a higher trust doesn't escape Sandboxed.
    #[test]
    fn import_registers_skill_at_sandboxed_in_trust_store() {
        let src = tempfile::tempdir().unwrap();
        let bundle = export_fixture(src.path());

        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        cmd_fleet_import(
            home,
            &bundle,
            ImportOpts {
                force: false,
                no_members: false,
                yes: true,
            },
        )
        .unwrap();

        // Load the trust store directly and assert there is an explicit entry
        // (not just the default fallback in get_trust_level).
        let trust = mur_common::trust::skills::SkillTrustStore::load(home).unwrap();
        let explicit = trust.entries.values().find(|e| e.name == "triage");
        assert!(
            explicit.is_some(),
            "skill 'triage' must have an explicit trust-store entry after import"
        );
        assert_eq!(
            explicit.unwrap().level,
            TrustLevel::Sandboxed,
            "imported skill must be registered at Sandboxed"
        );
    }

    /// I6 — Member identity: after --with-members import, the installed member's
    /// advertised pubkey in profile.yaml must equal its fresh local `identity.pub`,
    /// and must NOT equal the exporter's pubkey.
    #[test]
    fn import_with_members_advertised_pubkey_matches_fresh_local_key() {
        let src = tempfile::tempdir().unwrap();
        let s = src.path();
        let mur_dir = s.join("agents").join("mur");
        std::fs::create_dir_all(&mur_dir).unwrap();
        let concierge_id = mur_common::identity::AgentIdentity::generate();
        concierge_id.save(&mur_dir).unwrap();

        // Create a pm agent with a full profile (including identity block).
        let pm_dir = s.join("agents").join("pm");
        std::fs::create_dir_all(&pm_dir).unwrap();
        let exporter_pm_id = mur_common::identity::AgentIdentity::generate();
        exporter_pm_id.save(&pm_dir).unwrap();
        let exporter_pubkey = exporter_pm_id.public_key_multibase();
        // Build a full valid AgentProfile struct with the exporter's pubkey
        // in the identity block, then serialize to YAML so the bundler picks it up.
        let mut profile = mur_common::agent::AgentProfile::default_for_tests();
        profile.name = "pm".into();
        profile.identity.pubkey = exporter_pubkey.clone();
        profile.identity.key_version = 0;
        let profile_yaml = serde_yaml::to_string(&profile).unwrap();
        std::fs::write(pm_dir.join("profile.yaml"), &profile_yaml).unwrap();

        let fleet = Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "g".into(),
            router: None,
            members: vec!["pm".into()],
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
            team_id: None,
            parallel: None,
            requires_programs: vec![],
        };
        crate::cmd::fleet::store::save_fleet(s, &fleet).unwrap();
        let bundle = s.join("dev.fleet");
        crate::cmd::fleet::export::cmd_fleet_export(
            s,
            "dev",
            true,
            Some(bundle.clone()),
            "2026-06-20T00:00:00Z",
        )
        .unwrap();

        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        cmd_fleet_import(
            home,
            &bundle,
            ImportOpts {
                force: false,
                no_members: false,
                yes: true,
            },
        )
        .unwrap();

        // Read the installed profile and check the identity.pubkey.
        let installed_profile_bytes =
            std::fs::read(home.join("agents").join("pm").join("profile.yaml")).unwrap();
        let installed_profile: mur_common::agent::AgentProfile =
            serde_yaml::from_slice(&installed_profile_bytes).unwrap();
        let installed_pubkey = &installed_profile.identity.pubkey;

        // The advertised pubkey must NOT be the exporter's pubkey.
        assert_ne!(
            installed_pubkey, &exporter_pubkey,
            "installed profile must not advertise the exporter's pubkey"
        );
        // The advertised pubkey must match the locally generated identity.pub.
        let local_pub =
            std::fs::read_to_string(home.join("agents").join("pm").join("identity.pub")).unwrap();
        let local_pub = local_pub.trim();
        assert_eq!(
            installed_pubkey, local_pub,
            "installed profile identity.pubkey must match local identity.pub"
        );
    }

    /// I6 regression — malformed member profile: a bundle whose member profile.yaml
    /// cannot be parsed as AgentProfile must be refused (fail-closed), not silently
    /// written with a stale exporter key.
    #[test]
    fn import_with_members_refuses_malformed_member_profile() {
        use mur_common::fleet::Fleet;

        let src = tempfile::tempdir().unwrap();
        let s = src.path();

        // Set up exporting side: concierge + pm member with a MALFORMED profile.
        let mur_dir = s.join("agents").join("mur");
        std::fs::create_dir_all(&mur_dir).unwrap();
        let concierge_id = mur_common::identity::AgentIdentity::generate();
        concierge_id.save(&mur_dir).unwrap();

        let pm_dir = s.join("agents").join("pm");
        std::fs::create_dir_all(&pm_dir).unwrap();
        let pm_id = mur_common::identity::AgentIdentity::generate();
        pm_id.save(&pm_dir).unwrap();
        // Write a profile that is valid YAML but NOT a valid AgentProfile.
        std::fs::write(
            pm_dir.join("profile.yaml"),
            "totally: not_an_agent_profile\n",
        )
        .unwrap();

        let fleet = Fleet {
            name: "dev".into(),
            display_name: "Dev".into(),
            goal: "g".into(),
            router: None,
            members: vec!["pm".into()],
            channel_id: "fleet-dev".into(),
            rules: vec![],
            skills: vec![],
            loop_cfg: None,
            team_id: None,
            parallel: None,
            requires_programs: vec![],
        };
        crate::cmd::fleet::store::save_fleet(s, &fleet).unwrap();
        let bundle = s.join("dev.fleet");
        crate::cmd::fleet::export::cmd_fleet_export(
            s,
            "dev",
            true,
            Some(bundle.clone()),
            "2026-06-20T00:00:00Z",
        )
        .unwrap();

        let dst = tempfile::tempdir().unwrap();
        let home = dst.path();
        let err = cmd_fleet_import(
            home,
            &bundle,
            ImportOpts {
                force: false,
                no_members: false,
                yes: true,
            },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("malformed") || msg.contains("refusing to install member"),
            "expected malformed-profile refusal, got: {msg}"
        );
        // Nothing should be installed.
        assert!(
            !home.join("agents").join("pm").join("profile.yaml").exists(),
            "malformed member must not be installed"
        );
    }

    // ── Helper: build a tar.gz bundle from a manifest + file list ─────────────
    fn build_evil_bundle(
        manifest: &mur_common::fleet_bundle::BundleManifest,
        files: &[(String, Vec<u8>)],
    ) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        let mut buf = Vec::new();
        let gz = GzEncoder::new(&mut buf, Compression::default());
        let mut tar = tar::Builder::new(gz);
        let manifest_yaml = serde_yaml::to_string(manifest).unwrap();
        let add = |tar: &mut tar::Builder<_>, path: &str, data: &[u8]| {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, path, data).unwrap();
        };
        add(&mut tar, "bundle.yaml", manifest_yaml.as_bytes());
        for (p, b) in files {
            add(&mut tar, p, b);
        }
        tar.into_inner().unwrap().finish().unwrap();
        buf
    }
}
