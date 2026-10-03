//! `mur source sync` (one-shot and watch) and `install-schedule` handlers.

use anyhow::{Result, bail};

// ---------- Task 13 handlers ----------

pub(super) async fn sync(id: Option<&str>, full: bool) -> Result<()> {
    use crate::sources::adapters::obsidian::ObsidianAdapter;
    use crate::sources::instance::SourceInstanceStore;
    use crate::sources::sync::sync_source;
    use crate::store::embedding::EmbeddingConfig;
    use crate::store::vector::factory::get_vector_store;
    use anyhow::Context;

    let cfg = crate::store::config::load_config()?;
    let emb_cfg = EmbeddingConfig::from_config(&cfg);
    let index_path = dirs::home_dir()
        .context("no home dir")?
        .join(".mur")
        .join("index");
    let vector_store = get_vector_store(&cfg, &index_path).await?;
    let tantivy = crate::sources::tantivy::TantivyIndex::open_or_create(
        &dirs::home_dir().context("no home dir")?.join(".mur"),
    )?;

    let store = SourceInstanceStore::default_store()?;
    let targets: Vec<crate::sources::instance::SourceInstance> = match id {
        Some(i) => vec![store.load(i)?],
        None => store
            .list()?
            .into_iter()
            .filter(|inst| inst.enabled)
            .collect(),
    };
    if targets.is_empty() {
        println!("(no enabled sources to sync)");
        return Ok(());
    }

    let mut failed = 0usize;
    for mut inst in targets {
        if inst.type_name == "notion" {
            use crate::sources::adapters::notion::NotionAdapter;
            use crate::sources::credentials::{CredentialStore, OsKeyring, SERVICE};
            let kr = OsKeyring;
            let kr_account = inst
                .keyring_entry
                .clone()
                .unwrap_or_else(|| format!("{}:access_token", inst.id));
            let token = kr
                .get(SERVICE, &kr_account)?
                .ok_or_else(|| anyhow::anyhow!("no notion token in keyring for `{}`", inst.id))?;
            let adapter = NotionAdapter::from_instance(&inst, token)?;
            println!("↻ syncing {}{}", inst.id, if full { " (full)" } else { "" });
            let outcome = sync_source(
                &adapter,
                &mut inst,
                &store,
                vector_store.clone(),
                &tantivy,
                &emb_cfg,
                full,
            )
            .await;
            failed += print_sync_outcome(&inst.id, outcome);
            continue;
        }
        if inst.type_name == "joplin" {
            use crate::sources::adapters::joplin::JoplinAdapter;
            use crate::sources::credentials::{CredentialStore, OsKeyring, SERVICE};
            let token = if inst.scope.contains_key("server_url") {
                let kr = OsKeyring;
                let kr_account = inst
                    .keyring_entry
                    .clone()
                    .unwrap_or_else(|| format!("{}:api_token", inst.id));
                Some(
                    kr.get(SERVICE, &kr_account)?
                        .ok_or_else(|| anyhow::anyhow!("no joplin token for `{}`", inst.id))?,
                )
            } else {
                None
            };
            let adapter = JoplinAdapter::from_instance(&inst, token)?;
            println!("↻ syncing {}{}", inst.id, if full { " (full)" } else { "" });
            let outcome = sync_source(
                &adapter,
                &mut inst,
                &store,
                vector_store.clone(),
                &tantivy,
                &emb_cfg,
                full,
            )
            .await;
            failed += print_sync_outcome(&inst.id, outcome);
            continue;
        }
        if inst.type_name != "obsidian" {
            println!(
                "⏭  {}: adapter `{}` arrives in a later sub-milestone",
                inst.id, inst.type_name
            );
            continue;
        }
        let adapter = ObsidianAdapter::from_instance(&inst)?;
        println!("↻ syncing {}{}", inst.id, if full { " (full)" } else { "" });
        let outcome = sync_source(
            &adapter,
            &mut inst,
            &store,
            vector_store.clone(),
            &tantivy,
            &emb_cfg,
            full,
        )
        .await;
        failed += print_sync_outcome(&inst.id, outcome);
    }
    // Scripts, cron, and CI must be able to tell a sync that wrote nothing
    // from one that worked (#1614).
    if failed > 0 {
        bail!("{failed} source(s) synced with errors");
    }
    Ok(())
}

/// Print one source's sync result; returns 1 when it had any error.
fn print_sync_outcome(id: &str, outcome: Result<crate::sources::sync::SyncReport>) -> usize {
    let report = match outcome {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  ✗ {id}: {e:#}");
            return 1;
        }
    };
    println!(
        "  synced {} docs ({} chunks), deleted {}, {} errors",
        report.docs_synced,
        report.chunks_emitted,
        report.docs_deleted,
        report.errors.len()
    );
    for e in report.errors.iter().take(3) {
        println!("  ! {e}");
    }
    usize::from(!report.errors.is_empty())
}

// ---------- Task 7 handlers ----------

pub(super) async fn sync_watch() -> Result<()> {
    use crate::sources::instance::SourceInstanceStore;
    use crate::sources::tantivy::TantivyIndex;
    use crate::sources::watch::{WatchOptions, run_watch};
    use crate::store::embedding::EmbeddingConfig;
    use crate::store::vector::factory::get_vector_store;
    use anyhow::Context;

    let cfg = crate::store::config::load_config()?;
    let emb_cfg = EmbeddingConfig::from_config(&cfg);
    let index_path = dirs::home_dir()
        .context("no home dir")?
        .join(".mur")
        .join("index");
    let vector_store = get_vector_store(&cfg, &index_path).await?;
    let tantivy = TantivyIndex::open_or_create(&dirs::home_dir().unwrap().join(".mur"))?;
    let instance_store = SourceInstanceStore::default_store()?;
    run_watch(
        instance_store,
        vector_store,
        tantivy,
        emb_cfg,
        WatchOptions {
            poll_interval_secs: cfg.sources_global.poll_interval_secs,
        },
    )
    .await
}

// ---------- Task 8 handlers ----------

pub(super) async fn install_schedule() -> Result<()> {
    use anyhow::Context;
    #[cfg(target_os = "macos")]
    use std::io::Write;

    let cfg = crate::store::config::load_config()?;
    let interval_secs = cfg.sources_global.poll_interval_secs;

    let mur_path = std::env::current_exe().context("locate mur binary")?;
    let mur_path_str = mur_path.to_string_lossy().to_string();

    #[cfg(target_os = "macos")]
    {
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>run.mur.source-sync</string>
  <key>ProgramArguments</key>
  <array>
    <string>{mur_path_str}</string>
    <string>source</string>
    <string>sync</string>
  </array>
  <key>StartInterval</key><integer>{interval_secs}</integer>
  <key>StandardOutPath</key><string>/tmp/mur-source-sync.log</string>
  <key>StandardErrorPath</key><string>/tmp/mur-source-sync.err</string>
</dict>
</plist>
"#
        );
        let path = dirs::home_dir()
            .context("no home dir")?
            .join("Library/LaunchAgents/run.mur.source-sync.plist");
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let mut f = std::fs::File::create(&path)?;
        f.write_all(plist.as_bytes())?;
        println!("wrote {}", path.display());
        println!("Enable with: launchctl load -w {}", path.display());
        println!("Disable with: launchctl unload {}", path.display());
        Ok(())
    }

    #[cfg(target_os = "linux")]
    {
        let svc_dir = dirs::config_dir()
            .context("no config dir")?
            .join("systemd/user");
        std::fs::create_dir_all(&svc_dir)?;
        let svc_file = svc_dir.join("mur-source-sync.service");
        let timer_file = svc_dir.join("mur-source-sync.timer");

        let svc = format!(
            "[Unit]\nDescription=mur source sync\n\n[Service]\nType=oneshot\nExecStart={mur_path_str} source sync\n"
        );
        let timer = format!(
            "[Unit]\nDescription=Run mur source sync periodically\n\n[Timer]\nOnBootSec=1min\nOnUnitActiveSec={}s\nUnit=mur-source-sync.service\n\n[Install]\nWantedBy=timers.target\n",
            interval_secs
        );
        std::fs::write(&svc_file, svc)?;
        std::fs::write(&timer_file, timer)?;
        println!("wrote {} and {}", svc_file.display(), timer_file.display());
        println!("Enable with: systemctl --user enable --now mur-source-sync.timer");
        println!("Disable with: systemctl --user disable --now mur-source-sync.timer");
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        anyhow::bail!("install-schedule supported on macOS/Linux only");
    }
}
