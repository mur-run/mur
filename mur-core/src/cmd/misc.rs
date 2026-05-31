use anyhow::Result;
// KnowledgeBase removed: was only used by cmd_analyze (now deleted)
use mur_common::pattern::*;

use crate::store::yaml::YamlStore;

pub(crate) fn cmd_stats() -> Result<()> {
    use mur_common::knowledge::Maturity;

    let store = YamlStore::default_store()?;
    let patterns = store.list_all()?;

    let total = patterns.len();
    let mut session_count = 0;
    let mut project_count = 0;
    let mut core_count = 0;
    let mut active_count = 0;
    let mut deprecated_count = 0;
    let mut archived_count = 0;
    let mut draft_count = 0;
    let mut emerging_count = 0;
    let mut stable_count = 0;
    let mut canonical_count = 0;
    let mut total_importance = 0.0;
    let mut total_effectiveness = 0.0;
    let mut tracked_count = 0u64;
    let mut total_injections = 0u64;

    for p in &patterns {
        match p.tier {
            Tier::Session => session_count += 1,
            Tier::Project => project_count += 1,
            Tier::Core => core_count += 1,
        }
        match p.lifecycle.status {
            LifecycleStatus::Active => active_count += 1,
            LifecycleStatus::Deprecated => deprecated_count += 1,
            LifecycleStatus::Archived => archived_count += 1,
        }
        match p.maturity {
            Maturity::Draft => draft_count += 1,
            Maturity::Emerging => emerging_count += 1,
            Maturity::Stable => stable_count += 1,
            Maturity::Canonical => canonical_count += 1,
        }
        total_importance += p.importance;
        total_injections += p.evidence.injection_count;
        if p.evidence.injection_count > 0 {
            tracked_count += 1;
            total_effectiveness += p.evidence.effectiveness();
        }
    }

    let avg_importance = if total > 0 {
        total_importance / total as f64
    } else {
        0.0
    };
    let avg_effectiveness = if tracked_count > 0 {
        total_effectiveness / tracked_count as f64
    } else {
        0.0
    };

    println!("📊 MUR Core v2 Statistics");
    println!("─────────────────────────");
    println!("Total patterns:     {}", total);
    println!();
    println!("By tier:");
    println!("  📝 Session:       {}", session_count);
    println!("  📁 Project:       {}", project_count);
    println!("  ⭐ Core:          {}", core_count);
    println!();
    println!("By status:");
    println!("  ✅ Active:        {}", active_count);
    println!("  ⚠️  Deprecated:    {}", deprecated_count);
    println!("  📦 Archived:      {}", archived_count);
    println!();
    println!(
        "By maturity:        Draft: {} | Emerging: {} | Stable: {} | Canonical: {}",
        draft_count, emerging_count, stable_count, canonical_count
    );
    println!();
    println!("Avg importance:     {:.0}%", avg_importance * 100.0);
    println!("Total injections:   {}", total_injections);
    println!(
        "Tracked patterns:   {} / {} ({:.0}%)",
        tracked_count,
        total,
        if total > 0 {
            tracked_count as f64 / total as f64 * 100.0
        } else {
            0.0
        }
    );
    println!("Avg effectiveness:  {:.0}%", avg_effectiveness * 100.0);

    Ok(())
}

pub(crate) fn cmd_doctor() -> Result<()> {
    use mur_common::llm::is_reasoning_model;

    println!("🩺 MUR Doctor\n");

    // Check MUR directory
    let mur_dir = dirs::home_dir().map(|h| h.join(".mur")).unwrap_or_default();
    if mur_dir.exists() {
        println!("✅ MUR directory: {}", mur_dir.display());
    } else {
        println!("❌ MUR directory not found. Run `mur init` first.");
    }

    // Check patterns
    let store = YamlStore::default_store()?;
    let count = store.list_all()?.len();
    println!("✅ Patterns: {count}");

    // Check LLM config
    let config = crate::store::config::load_config()?;
    let model = &config.llm.model;
    if is_reasoning_model(model) {
        println!("✅ LLM model: {model} (recommended for session analysis)");
    } else {
        println!(
            "⚠️  LLM model: {model} (not ideal for session analysis — consider claude-opus-4-6, chatgpt-5.4, gemini-pro-3.5)"
        );
    }

    Ok(())
}

pub(crate) fn cmd_exchange_import(file: &str) -> Result<()> {
    let store = YamlStore::default_store()?;
    let path = std::path::Path::new(file);
    match crate::store::exchange::import_mkef_file(path, &store)? {
        Some(name) => println!("✅ Imported pattern: {}", name),
        None => println!("⏭️  Pattern already exists, skipped"),
    }
    Ok(())
}

pub(crate) fn cmd_exchange_import_all() -> Result<()> {
    let store = YamlStore::default_store()?;
    let exchange_dir = crate::store::exchange::default_exchange_dir();
    let imported = crate::store::exchange::import_mkef_dir(&exchange_dir, &store)?;
    if imported.is_empty() {
        println!("No new patterns to import from {}", exchange_dir.display());
    } else {
        println!("✅ Imported {} patterns:", imported.len());
        for name in &imported {
            println!("  - {}", name);
        }
    }
    Ok(())
}

pub(crate) fn cmd_exchange_export(name: &str, dir: Option<String>) -> Result<()> {
    let store = YamlStore::default_store()?;
    let pattern = store.get(name)?;
    let exchange_dir = dir
        .map(std::path::PathBuf::from)
        .unwrap_or_else(crate::store::exchange::default_exchange_dir);
    let path = crate::store::exchange::export_mkef(&pattern, &exchange_dir)?;
    println!("✅ Exported to {}", path.display());
    Ok(())
}

pub(crate) async fn cmd_login() -> Result<()> {
    if let Some(_tokens) = crate::auth::load_tokens() {
        println!("Already logged in. Run `mur logout` first to re-authenticate.");
        return Ok(());
    }

    println!("Logging in to mur community...");
    let client = reqwest::Client::new();
    let tokens = crate::auth::device_code_flow(&client).await?;
    crate::auth::save_tokens(&tokens)?;

    // Ping server to register device
    let base = crate::auth::server_url();
    let me_url = format!("{}/api/v1/core/auth/me", base);
    let req = crate::auth::auth_request(&client, reqwest::Method::GET, &me_url).await?;
    let _ = req.send().await;

    println!();
    println!("  ✅ Logged in successfully! Token stored in ~/.mur/auth.json");
    Ok(())
}

pub(crate) fn cmd_logout() -> Result<()> {
    crate::auth::clear_tokens()?;
    println!("Logged out. Auth tokens removed.");
    Ok(())
}
