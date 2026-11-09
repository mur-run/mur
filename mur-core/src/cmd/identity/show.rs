use std::path::Path;

use anyhow::{Context, Result, bail};
use mur_common::identity::encode_pubkey_bytes;
use mur_common::identity::human::{
    HumanIdentityError, human_key_dir, human_pubkey_path, human_rotations_path, rotation_chain,
};

pub(super) fn run(mur_home: &Path, json: bool) -> Result<()> {
    let chain = match rotation_chain(mur_home) {
        Ok(c) => c,
        Err(HumanIdentityError::NotEnrolled { .. }) => {
            bail!("no human identity yet; run `mur identity init`")
        }
        Err(e) => return Err(e).context("human identity"),
    };
    let pubkey = encode_pubkey_bytes(&chain.head_pubkey());
    let v = serde_json::json!({
        "pubkey": pubkey,
        "key_version": chain.head_key_version(),
        "rotations": chain.entries().len(),
        "public_key_file": human_pubkey_path(mur_home),
        "rotations_file": human_rotations_path(mur_home),
        "private_key_dir": human_key_dir(mur_home),
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!("public key:   {pubkey}");
        println!("key version:  {}", chain.head_key_version());
        println!("rotations:    {}", chain.entries().len());
        println!("public file:  {}", human_pubkey_path(mur_home).display());
        println!("chain file:   {}", human_rotations_path(mur_home).display());
        println!("private dir:  {}", human_key_dir(mur_home).display());
    }
    Ok(())
}
