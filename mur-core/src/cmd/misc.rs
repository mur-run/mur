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

    // Check skills
    let skills_dir = mur_dir.join("skills");
    let skill_count = if skills_dir.exists() {
        std::fs::read_dir(&skills_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .count()
    } else {
        0
    };
    if skill_count > 0 {
        println!("✅ Skills (installed): {skill_count}");
        if skill_count < 5 {
            println!("  ⚠ Few skills installed. Run `mur skill install <name>` to add more.");
        }
    } else {
        println!("  ⚠ No skills found in {}", skills_dir.display());
    }

    // Check LLM config
    let config = crate::store::config::load_config()?;
    let model = &config.llm.model;
    if is_reasoning_model(model) {
        println!("✅ LLM model: {model} (recommended for session analysis)");
    } else {
        println!(
            "⚠️  LLM model: {model} (not ideal for session analysis — consider claude-opus-5, chatgpt-5.4, gemini-pro-3.5)"
        );
    }

    // Check embedding provider (semantic search silently degrades without it)
    let emb = &config.embedding;
    match embedding_probe_addr(emb) {
        Some(addr) => {
            let reachable = addr
                .parse()
                .ok()
                .and_then(|a| {
                    std::net::TcpStream::connect_timeout(&a, std::time::Duration::from_secs(2)).ok()
                })
                .is_some();
            if reachable {
                println!(
                    "✅ Embedding: {} ({}) reachable at {addr}",
                    emb.provider, emb.model
                );
            } else {
                println!(
                    "❌ Embedding: {} ({}) NOT reachable at {addr} — semantic search is degraded.",
                    emb.provider, emb.model
                );
                if emb.provider == "ollama" {
                    println!(
                        "  ⚠ Install/start Ollama and run `ollama pull {}`.",
                        emb.model
                    );
                } else {
                    println!(
                        "  ⚠ Start the {} server, then run `mur internals reindex`.",
                        emb.provider
                    );
                }
            }
        }
        None => println!(
            "✅ Embedding: {} ({}) — remote provider, not probed",
            emb.provider, emb.model
        ),
    }

    report_mcp_pins(&mur_dir);

    Ok(())
}

/// Report agents whose pinned MCP binaries no longer match what's on disk.
///
/// B0 rule 6 refuses to start such an agent, so this is the difference between
/// finding out here and finding out when the agent won't come up. The signal
/// existed in `mur agent mcp inspect` all along and nothing was watching it —
/// one agent sat in drift for three days while `mur agent status` said healthy.
fn report_mcp_pins(mur_dir: &std::path::Path) {
    use crate::cmd::agent_mcp_pin::{InspectStatus, binary_status};

    let agents_dir = mur_dir.join("agents");
    let Ok(entries) = std::fs::read_dir(&agents_dir) else {
        return; // no agents yet — nothing to say
    };
    let mut dirs: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    dirs.sort(); // deterministic output

    let mut checked = 0usize;
    let mut problems: Vec<String> = Vec::new();

    for dir in dirs {
        let agent = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let Ok(yaml) = std::fs::read_to_string(dir.join("profile.yaml")) else {
            continue;
        };
        let Ok(profile) = serde_yaml_ng::from_str::<mur_common::agent::AgentProfile>(&yaml) else {
            continue;
        };
        for entry in &profile.mcp_servers {
            checked += 1;
            match binary_status(entry) {
                InspectStatus::Clean => {}
                InspectStatus::BinaryDrift => problems.push(format!(
                    "  ❌ {agent}/{name}: binary changed since install — this agent will REFUSE \
                     to start.\n     `mur agent mcp inspect {agent} --server {name}` to review, \
                     `mur agent mcp pin {agent} {name}` to re-approve.",
                    name = entry.name,
                )),
                InspectStatus::BinaryMissing => problems.push(format!(
                    "  ⚠ {agent}/{name}: binary not found ({command}).",
                    name = entry.name,
                    command = entry.command,
                )),
                InspectStatus::MissingPin => problems.push(format!(
                    "  ⚠ {agent}/{name}: unpinned (installed before pinning) — \
                     `mur agent mcp pin {agent} {name}` to start enforcing.",
                    name = entry.name,
                )),
                InspectStatus::InterpreterUnprotected => {
                    let launcher = entry
                        .command
                        .split_whitespace()
                        .next()
                        .unwrap_or(&entry.command);
                    match mur_common::mcp_package::parse_spec(&entry.command, &entry.args) {
                        // A floating spec is the sharp edge: the code that runs
                        // can change between two starts with no user action.
                        Some(spec) if spec.floats() => problems.push(format!(
                            "  ⚠ {agent}/{name}: `{launcher} {pkg}` has no pinned version — \
                             resolved fresh on every start.\n     \
                             `mur agent mcp pin {agent} {name}` records the version it resolves to now.",
                            name = entry.name,
                            pkg = spec.name,
                        )),
                        Some(spec) => problems.push(format!(
                            "  ⚠ {agent}/{name}: launched via `{launcher}` at {pkg} — version \
                             recorded, but the package contents are not verified.",
                            name = entry.name,
                            pkg = spec.to_arg(),
                        )),
                        None => problems.push(format!(
                            "  ⚠ {agent}/{name}: launched via `{launcher}` — the pin covers the \
                             interpreter, not the server code, so it is not enforced.",
                            name = entry.name,
                        )),
                    }
                }
                // Description-hash states need a live probe; `inspect --probe` owns those.
                _ => {}
            }
        }
    }

    if checked == 0 {
        return; // no MCP servers wired anywhere — silence beats a green tick
    }
    if problems.is_empty() {
        println!("✅ MCP pins: {checked} checked, all match");
    } else {
        println!(
            "MCP pins: {checked} checked, {} need attention",
            problems.len()
        );
        for p in &problems {
            println!("{p}");
        }
    }
}

/// host:port to TCP-probe for a LOCAL embedding provider; None for remote
/// providers (cloud APIs are auth-gated and shouldn't be probed blindly).
fn embedding_probe_addr(emb: &mur_common::config::EmbeddingConfig) -> Option<String> {
    let url = match emb.provider.as_str() {
        "ollama" => emb.ollama_endpoint.clone(),
        _ => emb.openai_url.clone()?,
    };
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let hostport = rest.split('/').next()?;
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()?),
        None => (hostport, if url.starts_with("https") { 443 } else { 80 }),
    };
    if host == "localhost" || host == "127.0.0.1" {
        Some(format!("127.0.0.1:{port}"))
    } else {
        None
    }
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
        println!("Already logged in. Run `mur auth logout` first to re-authenticate.");
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

#[cfg(test)]
mod doctor_tests {
    use super::embedding_probe_addr;
    use mur_common::config::EmbeddingConfig;

    #[test]
    fn probe_addr_local_remote_and_default_port() {
        let mut e = EmbeddingConfig::default(); // ollama @ localhost:11434
        assert_eq!(embedding_probe_addr(&e).as_deref(), Some("127.0.0.1:11434"));

        e.provider = "omlx".into();
        e.openai_url = Some("http://127.0.0.1:8000/v1".into());
        assert_eq!(embedding_probe_addr(&e).as_deref(), Some("127.0.0.1:8000"));

        // Remote providers are never probed.
        e.openai_url = Some("https://api.openai.com/v1".into());
        assert_eq!(embedding_probe_addr(&e), None);

        // openai-style provider without a URL → nothing to probe.
        e.openai_url = None;
        assert_eq!(embedding_probe_addr(&e), None);
    }
}
