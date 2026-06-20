//! `mur fleet import`: verify a `.fleet` bundle (untrusted observed data),
//! security-scan its skills, confirm, then install. Never auto-runs the fleet.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use mur_common::fleet::Fleet;
use mur_common::fleet_bundle::{BundleManifest, content_hash, verify_manifest_sig};
use mur_common::skill::manifest::{SkillManifest, SkillScope};
use mur_common::skill::types::TrustLevel;

use super::bundle_transport::{FleetBundleTransport, LocalFile};
use super::store;

pub struct ImportOpts {
    pub force: bool,
    pub no_members: bool,
    pub yes: bool,
}

/// Unpack the tar.gz into (manifest, path->bytes). Rejects unsafe entry paths.
pub(crate) fn unpack_for_test(bytes: &[u8]) -> Result<(BundleManifest, HashMap<String, Vec<u8>>)> {
    let gz = GzDecoder::new(bytes);
    let mut ar = tar::Archive::new(gz);
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    for entry in ar.entries().context("read archive")? {
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

pub fn cmd_fleet_import(mur_home: &Path, file: &Path, opts: ImportOpts) -> Result<()> {
    // 1. Read + unpack (transport seam).
    let src = file.to_str().context("bundle path is not UTF-8")?;
    let bytes = LocalFile.read(src)?;
    let (manifest, files) = unpack_for_test(&bytes)?;

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

    // 4. Provenance + plan (two-tier trust: Phase A pins an empty official set, so
    //    every bundle is a peer/TOFU import → lowest trust, scan + confirm).
    let skill_paths: Vec<&String> = manifest
        .entries
        .iter()
        .map(|e| &e.path)
        .filter(|p| p.starts_with("skills/"))
        .collect();
    println!(
        "Fleet bundle '{}' from signer {}",
        manifest.fleet_name, manifest.signer_fingerprint
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
    let mut parsed_skills: Vec<(String, SkillManifest)> = Vec::new();
    for path in &skill_paths {
        let m: SkillManifest =
            serde_yaml::from_slice(&files[*path]).with_context(|| format!("parse {path}"))?;
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

    // 6. HITL confirm — nothing written before approval.
    if !confirm("Install this fleet + skills?", opts.yes)? {
        bail!("import cancelled");
    }

    // 7. Conflict checks (fail-closed unless --force).
    if store::fleet_path(mur_home, &manifest.fleet_name).is_file() && !opts.force {
        bail!(
            "fleet '{}' already exists — re-run with --force to overwrite",
            manifest.fleet_name
        );
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
        mur_common::skill::local::set_trust_level(mur_home, &name, TrustLevel::Sandboxed)
            .map_err(|e| anyhow::anyhow!("set trust {name}: {e}"))?;
    }

    // 9. Install the fleet definition.
    let fleet: Fleet = serde_yaml::from_slice(
        files
            .get("fleet.yaml")
            .context("bundle missing fleet.yaml")?,
    )
    .context("parse fleet.yaml")?;
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
    Ok(())
}

/// Task 4 fills this in. Stub: bundled-member install not yet implemented.
pub(crate) fn install_bundled_members(
    _mur_home: &Path,
    _manifest: &BundleManifest,
    _files: &HashMap<String, Vec<u8>>,
    _force: bool,
    _yes: bool,
) -> Result<()> {
    bail!("bundled-member install not yet implemented");
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
        // flip a byte in the archive
        let mut bytes = std::fs::read(&bundle).unwrap();
        let n = bytes.len();
        bytes[n / 2] ^= 0xFF;
        let bad = src.path().join("bad.fleet");
        std::fs::write(&bad, &bytes).unwrap();

        let dst = tempfile::tempdir().unwrap();
        let err = cmd_fleet_import(
            dst.path(),
            &bad,
            ImportOpts {
                force: false,
                no_members: false,
                yes: true,
            },
        )
        .unwrap_err();
        // Any rejection error is acceptable: sig/hash mismatch, gzip/archive corruption.
        let msg = format!("{err:#}").to_lowercase();
        assert!(
            msg.contains("verif")
                || msg.contains("hash")
                || msg.contains("gzip")
                || msg.contains("archive")
                || msg.contains("deflate")
                || msg.contains("corrupt")
                || msg.contains("invalid")
                || msg.contains("mismatch")
                || msg.contains("signature")
                || msg.contains("failed")
                || msg.contains("error"),
            "unexpected error: {msg}"
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
}
