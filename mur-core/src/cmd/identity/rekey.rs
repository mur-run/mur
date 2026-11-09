//! Rotate the human owner key. Same shape as `agent_rekey.rs`, except the old
//! secret goes to `keys/.human/identity.key.prev` by explicit path (the agent
//! flow derives it from the agent dir, which would land it in `identity/`),
//! and there is no `--emergency`: an unsigned owner rotation would let anyone
//! with write access to `identity/` take over.

use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use mur_common::identity::human::{
    HumanIdentityError, human_identity_dir, human_key_dir, human_private_key_path,
    human_pubkey_path, human_rotations_path, rotation_chain,
};
use mur_common::identity::{AgentIdentity, RotationAttestation, RotationReason};

use crate::identity_enroll::HumanSigner;

const SCRATCH: &str = ".rekey-scratch";

fn parse_reason(s: &str) -> Result<RotationReason> {
    match s {
        "scheduled" => Ok(RotationReason::Scheduled),
        "suspect-compromise" => Ok(RotationReason::SuspectCompromise),
        "owner-change" => Ok(RotationReason::OwnerChange),
        "emergency" => bail!("the human identity has no emergency rekey: it needs the old key"),
        other => {
            bail!("unknown reason '{other}' (allowed: scheduled, suspect-compromise, owner-change)")
        }
    }
}

pub(super) fn run(mur_home: &Path, reason: &str, yes: bool) -> Result<()> {
    let reason = parse_reason(reason)?;
    let chain = match rotation_chain(mur_home) {
        Ok(c) => c,
        Err(HumanIdentityError::NotEnrolled { .. }) => {
            bail!("no human identity to rotate; run `mur identity init` first")
        }
        Err(e) => return Err(e).context("human identity"),
    };
    let signer = HumanSigner::load(mur_home)?;
    if signer.pubkey() != chain.head_pubkey() {
        bail!("the private key does not match the chain head; restore the matching key first");
    }
    let old_version = chain.head_key_version();
    let new_version = old_version + 1;
    let old_pub = mur_common::identity::encode_pubkey_bytes(&chain.head_pubkey());

    if !yes {
        eprintln!("About to rotate the human owner key:");
        eprintln!("  current key_version: {old_version}");
        eprintln!("  current pubkey:      {old_pub}");
        eprintln!("  next key_version:    {new_version}");
        eprint!("\nProceed? [y/N] ");
        std::io::stderr().flush().ok();
        let mut buf = String::new();
        std::io::stdin().read_line(&mut buf).ok();
        if !buf.trim().eq_ignore_ascii_case("y") {
            eprintln!("aborted");
            return Ok(());
        }
    }

    let new_identity = AgentIdentity::generate();
    let new_pub = new_identity.pubkey_text();
    let mut att = RotationAttestation::new(
        uuid::Uuid::now_v7().to_string(),
        &old_pub,
        &new_pub,
        old_version,
        new_version,
        chrono::Utc::now().to_rfc3339(),
        reason,
    );
    att.sign(signer.signing_key());
    rotate_files(mur_home, &new_identity, &att)?;
    println!("rekey ok: key_version {old_version} -> {new_version}");
    println!("  pubkey:   {new_pub}");
    println!(
        "  previous key kept at {}",
        human_key_dir(mur_home).join("identity.key.prev").display()
    );
    Ok(())
}

/// Order matters for crash safety: the old secret is set aside before the new
/// one lands, and the public half is promoted last, so `identity.pub` never
/// names a key whose secret is missing. A crash in between leaves a state
/// `rotation_chain` reports as invalid rather than one that verifies wrongly.
fn rotate_files(mur_home: &Path, new: &AgentIdentity, att: &RotationAttestation) -> Result<()> {
    let key_dir = human_key_dir(mur_home);
    let scratch = key_dir.join(SCRATCH);
    if scratch.exists() {
        fs::remove_dir_all(&scratch).ok();
    }
    new.save_split(&scratch, &scratch)
        .context("save the new key to scratch")?;

    let key = human_private_key_path(mur_home);
    let prev = key_dir.join("identity.key.prev");
    if prev.exists() {
        fs::remove_file(&prev).with_context(|| format!("remove {}", prev.display()))?;
    }
    fs::rename(&key, &prev).with_context(|| format!("move {} aside", key.display()))?;
    fs::rename(scratch.join("identity.key"), &key).context("install the new private key")?;

    let line = serde_json::to_string(att).context("serialize attestation")?;
    let rotations = human_rotations_path(mur_home);
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(&rotations)
        .with_context(|| format!("open {}", rotations.display()))?;
    writeln!(f, "{line}")?;
    f.sync_all().ok();

    let pub_path = human_pubkey_path(mur_home);
    debug_assert!(pub_path.starts_with(human_identity_dir(mur_home)));
    fs::rename(scratch.join("identity.pub"), &pub_path).context("promote the new public key")?;
    fs::remove_dir_all(&scratch).ok();
    Ok(())
}
