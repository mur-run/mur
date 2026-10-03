//! `mur source add <kind>` handlers.

use anyhow::{Result, bail};

pub(super) async fn add_obsidian(
    instance: Option<String>,
    vault: std::path::PathBuf,
    exclude_folder: Vec<String>,
) -> Result<()> {
    use crate::sources::instance::{SourceInstance, SourceInstanceStore, SourceStats, SyncState};
    use crate::sources::kind::SourceKind;
    use anyhow::Context;
    use std::collections::BTreeMap;

    let store = SourceInstanceStore::default_store()?;
    let id = match instance {
        Some(tag) if !tag.is_empty() => format!("obsidian:{tag}"),
        _ => {
            let existing: Vec<String> = store.list()?.into_iter().map(|i| i.id).collect();
            if !existing.iter().any(|id| id == "obsidian") {
                "obsidian".to_string()
            } else {
                let mut rng: u16 = rand::random();
                loop {
                    let candidate = format!("obsidian:{rng:04x}");
                    if !existing.contains(&candidate) {
                        break candidate;
                    }
                    rng = rng.wrapping_add(1);
                }
            }
        }
    };

    let abs_vault = std::fs::canonicalize(&vault)
        .with_context(|| format!("resolve vault path {}", vault.display()))?;
    if !abs_vault.is_dir() {
        bail!("vault path is not a directory: {}", abs_vault.display());
    }

    let mut scope: BTreeMap<String, serde_yaml::Value> = BTreeMap::new();
    scope.insert(
        "vault".into(),
        serde_yaml::Value::String(abs_vault.to_string_lossy().to_string()),
    );
    if !exclude_folder.is_empty() {
        scope.insert(
            "exclude_folders".into(),
            serde_yaml::Value::Sequence(
                exclude_folder
                    .into_iter()
                    .map(serde_yaml::Value::String)
                    .collect(),
            ),
        );
    }

    let inst = SourceInstance {
        id: id.clone(),
        type_name: "obsidian".into(),
        kind: SourceKind::PullIndex,
        enabled: true,
        weight: 1.0,
        scope,
        sync: SyncState::default(),
        stats: SourceStats::default(),
        keyring_entry: None,
    };
    store.save(&inst)?;
    println!("✅ Connected vault {} as `{}`", abs_vault.display(), id);
    println!("Run `mur source sync {id}` to index.");
    Ok(())
}

pub(super) async fn add_notion(
    instance: Option<String>,
    workspace: Option<String>,
    token: Option<String>,
) -> Result<()> {
    use crate::sources::adapters::notion::{OAuthResult, run_oauth_flow};
    use crate::sources::credentials::{CredentialStore, OsKeyring, SERVICE, account};
    use crate::sources::instance::{SourceInstance, SourceInstanceStore, SourceStats, SyncState};
    use crate::sources::kind::SourceKind;
    use anyhow::Context;
    use std::collections::BTreeMap;

    let store = SourceInstanceStore::default_store()?;
    let id = match instance {
        Some(tag) if !tag.is_empty() => format!("notion:{tag}"),
        _ => {
            let existing: Vec<String> = store.list()?.into_iter().map(|i| i.id).collect();
            if !existing.iter().any(|s| s == "notion") {
                "notion".to_string()
            } else {
                let mut rng: u16 = rand::random();
                loop {
                    let candidate = format!("notion:{rng:04x}");
                    if !existing.contains(&candidate) {
                        break candidate;
                    }
                    rng = rng.wrapping_add(1);
                }
            }
        }
    };

    let (access_token, workspace_id, workspace_name) = if let Some(pat) = token {
        (pat, workspace, None::<String>)
    } else {
        println!("-> launching Notion OAuth (PKCE) flow...");
        let OAuthResult {
            access_token,
            workspace_id,
            workspace_name,
        } = run_oauth_flow().await?;
        (access_token, workspace_id, workspace_name)
    };

    // Persist credentials to keyring
    let keyring = OsKeyring;
    let kr_account = account(&id, "access_token");
    keyring
        .set(SERVICE, &kr_account, &access_token)
        .context("store notion access_token in keyring")?;

    let mut scope: BTreeMap<String, serde_yaml::Value> = BTreeMap::new();
    if let Some(w) = workspace_id {
        scope.insert("workspace_id".into(), serde_yaml::Value::String(w));
    }
    if let Some(n) = workspace_name {
        scope.insert("workspace_name".into(), serde_yaml::Value::String(n));
    }

    let inst = SourceInstance {
        id: id.clone(),
        type_name: "notion".into(),
        kind: SourceKind::PullIndex,
        enabled: true,
        weight: 1.0,
        scope,
        sync: SyncState::default(),
        stats: SourceStats::default(),
        keyring_entry: Some(kr_account.clone()),
    };
    store.save(&inst)?;
    println!("Connected Notion as `{id}`");
    println!("Run `mur source sync {id}` to index.");
    Ok(())
}

pub(super) async fn add_joplin(
    instance: Option<String>,
    db: Option<std::path::PathBuf>,
    server: Option<String>,
    token: Option<String>,
) -> Result<()> {
    use crate::sources::credentials::{CredentialStore, OsKeyring, SERVICE, account};
    use crate::sources::instance::{SourceInstance, SourceInstanceStore, SourceStats, SyncState};
    use crate::sources::kind::SourceKind;
    use anyhow::Context;
    use std::collections::BTreeMap;

    let store = SourceInstanceStore::default_store()?;
    let id = match instance {
        Some(tag) if !tag.is_empty() => format!("joplin:{tag}"),
        _ => "joplin".to_string(),
    };

    let mut scope: BTreeMap<String, serde_yaml::Value> = BTreeMap::new();
    let mut keyring_entry: Option<String> = None;
    if let Some(srv) = server {
        let tok = token.context("joplin --server requires --token")?;
        scope.insert("server_url".into(), serde_yaml::Value::String(srv));
        let kr = OsKeyring;
        let kr_account = account(&id, "api_token");
        kr.set(SERVICE, &kr_account, &tok)?;
        keyring_entry = Some(kr_account);
    } else if let Some(db_path) = db {
        let abs = std::fs::canonicalize(&db_path)
            .with_context(|| format!("resolve {}", db_path.display()))?;
        scope.insert(
            "db_path".into(),
            serde_yaml::Value::String(abs.to_string_lossy().to_string()),
        );
    } else {
        bail!(
            "specify --db <path> for local SQLite or --server <url> --token <pat> for Joplin Server"
        );
    }

    let inst = SourceInstance {
        id: id.clone(),
        type_name: "joplin".into(),
        kind: SourceKind::PullIndex,
        enabled: true,
        weight: 1.0,
        scope,
        sync: SyncState::default(),
        stats: SourceStats::default(),
        keyring_entry,
    };
    store.save(&inst)?;
    println!("Connected Joplin as `{id}`");
    println!("Run `mur source sync {id}` to index.");
    Ok(())
}
