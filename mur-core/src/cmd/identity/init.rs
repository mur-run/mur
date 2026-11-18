use std::path::Path;

use anyhow::Result;
use mur_common::identity::{encode_pubkey_bytes, human::human_pubkey_path};

use crate::identity_enroll::{Enrollment, enroll};

pub(super) fn run(mur_home: &Path) -> Result<()> {
    let outcome = enroll(mur_home)?;
    let key = encode_pubkey_bytes(&outcome.pubkey());
    match outcome {
        Enrollment::Created { .. } => println!("created the human identity: {key}"),
        Enrollment::Existing { .. } => println!("human identity already exists: {key}"),
    }
    println!("  public key: {}", human_pubkey_path(mur_home).display());
    Ok(())
}
