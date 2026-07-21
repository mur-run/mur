//! `mur fleet export`: package a fleet's definition + its fleet-scoped skills
//! (+ optional member agents — Task 4) into a signed `.fleet` bundle.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use flate2::Compression;
use flate2::write::GzEncoder;
use mur_common::fleet_bundle::{
    BundleEntry, BundleManifest, FLEET_BUNDLE_FORMAT, content_hash, manifest_sign_input,
    signer_fingerprint,
};
use mur_common::identity::AgentIdentity;
use mur_common::skill::manifest::{SkillManifest, SkillScope};

use super::store;

/// Installed skills whose scope targets exactly this fleet.
pub fn collect_fleet_skills(
    mur_home: &Path,
    fleet_name: &str,
) -> Result<Vec<(String, SkillManifest)>> {
    let mut out = Vec::new();
    for name in mur_common::skill::local::list_installed(mur_home).unwrap_or_default() {
        let Ok(m) = mur_common::skill::local::load_installed(mur_home, &name) else {
            continue;
        };
        if m.scope == SkillScope::Fleet && m.fleet.as_deref() == Some(fleet_name) {
            out.push((name, m));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0)); // deterministic order
    Ok(out)
}

/// Add one in-memory blob to a tar builder at `path`.
fn add_blob<W: Write>(tar: &mut tar::Builder<W>, path: &str, data: &[u8]) -> Result<()> {
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    tar.append_data(&mut h, path, data)
        .with_context(|| format!("tar add {path}"))?;
    Ok(())
}

/// Build the `.fleet` (tar.gz) bytes from a manifest + the (path, bytes) files.
fn build_bundle_bytes(manifest: &BundleManifest, files: &[(String, Vec<u8>)]) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    {
        let gz = GzEncoder::new(&mut buf, Compression::default());
        let mut tar = tar::Builder::new(gz);
        let manifest_yaml = serde_yaml::to_string(manifest).context("serialize manifest")?;
        add_blob(&mut tar, "bundle.yaml", manifest_yaml.as_bytes())?;
        for (path, bytes) in files {
            add_blob(&mut tar, path, bytes)?;
        }
        tar.into_inner()
            .context("finish tar")?
            .finish()
            .context("flush gzip")?;
    }
    Ok(buf)
}

pub fn cmd_fleet_export(
    mur_home: &Path,
    name: &str,
    with_members: bool,
    out: Option<PathBuf>,
    now_rfc3339: &str,
) -> Result<()> {
    let fleet = store::load_fleet(mur_home, name)?;

    // 1. Collect files: fleet.yaml (host-specific .last_run/.stopped are separate
    //    sentinel files, never in fleet.yaml, so nothing to strip) + scoped skills.
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let fleet_yaml = serde_yaml::to_string(&fleet).context("serialize fleet")?;
    files.push(("fleet.yaml".to_string(), fleet_yaml.into_bytes()));

    for (skill_name, m) in collect_fleet_skills(mur_home, name)? {
        let yaml = serde_yaml::to_string(&m).context("serialize skill")?;
        files.push((format!("skills/{skill_name}/skill.yaml"), yaml.into_bytes()));
    }

    // 2. Members (Task 4 fills this in when with_members=true).
    if with_members {
        add_member_exports(mur_home, &fleet, &mut files)?;
    }

    // 3. Manifest: pin every file by hash, sign with the concierge identity.
    let id = AgentIdentity::load(&mur_home.join("agents").join("mur"))
        .context("load concierge identity (~/.mur/agents/mur)")?;
    let signer_pubkey = id.public_key_multibase();
    let entries: Vec<BundleEntry> = files
        .iter()
        .map(|(p, b)| BundleEntry {
            path: p.clone(),
            sha256: content_hash(b),
        })
        .collect();
    let mut manifest = BundleManifest {
        format_version: FLEET_BUNDLE_FORMAT,
        fleet_name: name.to_string(),
        created_at: now_rfc3339.to_string(),
        signer_fingerprint: signer_fingerprint(&signer_pubkey),
        signer_pubkey,
        includes_members: with_members,
        members: fleet.members.clone(),
        entries,
        sig: None,
        distribution: None,
    };
    let input = manifest_sign_input(&manifest);
    manifest.sig = Some(multibase::encode(
        multibase::Base::Base58Btc,
        id.sign_bytes(&input),
    ));

    // 4. Build + write via transport.
    let bytes = build_bundle_bytes(&manifest, &files)?;
    let out = out.unwrap_or_else(|| PathBuf::from(format!("{name}.fleet")));
    let out_str = out.to_str().context("output path is not UTF-8")?;
    std::fs::write(&out, &bytes).with_context(|| format!("write bundle {out_str}"))?;

    let skill_count = manifest
        .entries
        .iter()
        .filter(|e| e.path.starts_with("skills/"))
        .count();
    println!(
        "Exported fleet '{name}' → {} (signer {}, {} skill(s){})",
        out.display(),
        manifest.signer_fingerprint,
        skill_count,
        if with_members {
            ", members included"
        } else {
            ""
        }
    );
    Ok(())
}

/// Bundle each member's `profile.yaml` (never the private `identity.key`).
pub(crate) fn add_member_exports(
    mur_home: &Path,
    fleet: &mur_common::fleet::Fleet,
    files: &mut Vec<(String, Vec<u8>)>,
) -> Result<()> {
    for member in &fleet.members {
        let canon = crate::a2a_dial::canonicalize_agent_name(mur_home, member);
        let profile = mur_home.join("agents").join(&canon).join("profile.yaml");
        if !profile.is_file() {
            // Member not present locally — skip silently (import will report missing).
            continue;
        }
        let body = std::fs::read(&profile)
            .with_context(|| format!("read profile {}", profile.display()))?;
        files.push((format!("members/{canon}/profile.yaml"), body));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::fleet::Fleet;
    use mur_common::fleet_bundle::{content_hash, verify_manifest_sig};
    use mur_common::identity::AgentIdentity;
    use mur_common::skill::manifest::{SkillManifest, SkillScope};

    fn seed_concierge(home: &std::path::Path) {
        let dir = home.join("agents").join("mur");
        std::fs::create_dir_all(&dir).unwrap();
        AgentIdentity::generate().save(&dir).unwrap();
    }

    fn seed_skill(home: &std::path::Path, name: &str, scope: SkillScope, fleet: Option<&str>) {
        let mut m = SkillManifest {
            name: name.to_string(),
            scope,
            fleet: fleet.map(str::to_string),
            ..test_manifest(name)
        };
        m.scope = scope; // explicit
        let dir = mur_common::skill::store::global_skill_dir(home, name);
        mur_common::skill::store::write_to_dir(&dir, &m).unwrap();
    }

    // Minimal valid manifest builder (fill required fields per SkillManifest).
    fn test_manifest(name: &str) -> SkillManifest {
        serde_yaml::from_str(&format!(
            "name: {name}\nversion: 1.0.0\npublisher: human:t\ndescription: t\n\
             category: context\ncontent:\n  abstract: a\n  context: body\n"
        ))
        .unwrap()
    }

    #[test]
    fn collect_fleet_skills_filters_by_scope_and_fleet() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        seed_skill(home, "in", SkillScope::Fleet, Some("dev"));
        seed_skill(home, "other", SkillScope::Fleet, Some("ops"));
        seed_skill(home, "userk", SkillScope::User, None);
        let got = collect_fleet_skills(home, "dev").unwrap();
        let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["in"]);
    }

    #[test]
    fn export_with_members_bundles_profile_without_private_key() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        seed_concierge(home);
        // member agent "pm" with a profile + a private key
        let pm = home.join("agents").join("pm");
        std::fs::create_dir_all(&pm).unwrap();
        std::fs::write(pm.join("profile.yaml"), "name: pm\nentitlements: {}\n").unwrap();
        AgentIdentity::generate().save(&pm).unwrap(); // writes identity.key
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
        crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();
        let out = home.join("dev.fleet");
        cmd_fleet_export(home, "dev", true, Some(out.clone()), "2026-06-20T00:00:00Z").unwrap();

        let bytes = std::fs::read(&out).unwrap();
        let (manifest, files) = crate::cmd::fleet::import::unpack_bundle(&bytes).unwrap();
        assert!(manifest.includes_members);
        assert!(files.contains_key("members/pm/profile.yaml"));
        // NO private key travels
        assert!(!files.keys().any(|k| k.contains("identity.key")));
    }

    #[test]
    fn export_writes_a_verifiable_signed_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        seed_concierge(home);
        seed_skill(home, "triage", SkillScope::Fleet, Some("dev"));
        // a minimal fleet
        let fleet = Fleet {
            name: "dev".into(),
            display_name: String::new(),
            goal: "ship".into(),
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
        crate::cmd::fleet::store::save_fleet(home, &fleet).unwrap();

        let out = home.join("dev.fleet");
        cmd_fleet_export(
            home,
            "dev",
            false,
            Some(out.clone()),
            "2026-06-20T00:00:00Z",
        )
        .unwrap();
        assert!(out.is_file());

        // re-open: unpack, read manifest, verify signature + entry hashes
        let bytes = std::fs::read(&out).unwrap();
        let (manifest, files) = crate::cmd::fleet::import::unpack_bundle(&bytes).unwrap();
        let (_, pk) = multibase::decode(&manifest.signer_pubkey).unwrap();
        let pk: [u8; 32] = pk.try_into().unwrap();
        assert!(verify_manifest_sig(&manifest, &pk));
        assert!(manifest.entries.iter().any(|e| e.path == "fleet.yaml"));
        assert!(
            manifest
                .entries
                .iter()
                .any(|e| e.path == "skills/triage/skill.yaml")
        );
        // every entry hash matches the unpacked file bytes
        for e in &manifest.entries {
            assert_eq!(content_hash(&files[&e.path]), e.sha256);
        }
        assert!(!manifest.includes_members);
        assert_eq!(manifest.members, vec!["pm".to_string()]);
    }
}
