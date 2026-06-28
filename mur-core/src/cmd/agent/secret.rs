//! `mur agent secret` — set / delete / list per-agent keychain secrets.

use anyhow::{Context, Result};

use super::load_profile_for_edit;

pub(crate) const SECRET_SERVICE: &str = "mur-agent";

pub async fn cmd_secret_set(agent: &str, key: &str, value: Option<&str>) -> Result<()> {
    use mur_common::secret::keychain_set;
    let val: String = match value {
        Some(v) => v.to_string(),
        None => {
            eprint!("Enter value for {key} (input hidden): ");
            rpassword::read_password().context("read hidden value")?
        }
    };
    let acct = format!("{agent}/{key}");
    keychain_set(SECRET_SERVICE, &acct, &val)
        .await
        .with_context(|| format!("set {SECRET_SERVICE}/{acct}"))?;
    println!("Wrote {SECRET_SERVICE}/{acct}");
    Ok(())
}

pub async fn cmd_secret_delete(agent: &str, key: &str) -> Result<()> {
    use mur_common::secret::keychain_delete;
    let acct = format!("{agent}/{key}");
    keychain_delete(SECRET_SERVICE, &acct)
        .await
        .with_context(|| format!("delete {SECRET_SERVICE}/{acct}"))?;
    println!("Deleted {SECRET_SERVICE}/{acct}");
    Ok(())
}

pub async fn cmd_secret_list(agent: &str) -> Result<()> {
    use mur_common::model::ModelRegistry;
    let (_path, profile) = load_profile_for_edit(agent)?;
    match profile.model_ref.as_deref() {
        Some(name) => {
            let reg_path = ModelRegistry::default_path()?;
            let reg = ModelRegistry::load_from(&reg_path)
                .with_context(|| format!("load registry {}", reg_path.display()))?;
            match reg.models.get(name) {
                Some(entry) => match &entry.secret {
                    Some(s) => {
                        let ok = s.check().await;
                        println!(
                            "{} ({}) — {}",
                            s,
                            name,
                            if ok { "present" } else { "missing" }
                        );
                    }
                    None => println!("{name}: no secret configured"),
                },
                None => println!("model_ref '{name}' not found in registry"),
            }
        }
        None => println!("agent uses inline model — no registry secret"),
    }
    Ok(())
}
