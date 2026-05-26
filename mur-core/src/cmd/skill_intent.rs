//! `mur skill intent {canonicalise,show}` CLI (M7c).

use std::path::Path;

use anyhow::Result;

use crate::cross_agent::intent::canonical::{
    build_canonical, read_canonical_yaml, write_canonical_yaml,
};

pub fn cmd_intent_canonicalise(
    home: &Path,
    generated_by: &str,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    let ic = build_canonical(home, generated_by)?;
    if dry_run {
        if json {
            serde_json::to_writer_pretty(std::io::stdout(), &ic)?;
            println!();
        } else {
            println!("{}", serde_yaml_ng::to_string(&ic)?);
        }
        return Ok(());
    }
    write_canonical_yaml(home, &ic)?;
    println!(
        "wrote {} cluster(s) to {}",
        ic.canonical.len(),
        home.join("intent_canonical.yaml").display()
    );
    Ok(())
}

pub fn cmd_intent_show(home: &Path, json: bool) -> Result<()> {
    match read_canonical_yaml(home)? {
        None => {
            eprintln!(
                "no canonical mapping at {}",
                home.join("intent_canonical.yaml").display()
            );
            std::process::exit(2);
        }
        Some(ic) => {
            if json {
                serde_json::to_writer_pretty(std::io::stdout(), &ic)?;
                println!();
            } else {
                println!("{}", serde_yaml_ng::to_string(&ic)?);
            }
        }
    }
    Ok(())
}
