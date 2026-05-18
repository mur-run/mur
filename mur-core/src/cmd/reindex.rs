use anyhow::Result;

use crate::store::workflow_yaml::WorkflowYamlStore;
use crate::store::yaml::{YamlStore, default_mur_dir};

pub(crate) async fn cmd_reindex() -> Result<()> {
    use crate::store::embedding::{EmbeddingConfig, embed};
    use crate::store::vector::LanceDbStore as VectorStore;

    let pattern_store = YamlStore::default_store()?;
    let patterns = pattern_store.list_all()?;
    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    if patterns.is_empty() && workflows.is_empty() {
        println!("No patterns or workflows to index.");
        return Ok(());
    }

    let cfg = crate::store::config::load_config()?;
    let config = EmbeddingConfig::from_config(&cfg);
    let index_path = dirs::home_dir()
        .expect("no home dir")
        .join(".mur")
        .join("index");

    println!(
        "🔄 Reindexing {} patterns + {} workflows using {} ({})...",
        patterns.len(),
        workflows.len(),
        config.model,
        match &config.provider {
            crate::store::embedding::EmbeddingProvider::Ollama { base_url } => base_url.clone(),
            crate::store::embedding::EmbeddingProvider::OpenAI { .. } => "OpenAI".into(),
        }
    );

    let mut indexed_patterns = Vec::new();
    let mut indexed_workflows = Vec::new();
    let mut errors = 0;
    let total = patterns.len() + workflows.len();

    for (i, pattern) in patterns.iter().enumerate() {
        let mut text = format!(
            "{}: {}\n{}",
            pattern.name,
            pattern.description,
            pattern.content.as_text()
        );
        // Include attachment descriptions in embedding text for better search
        for att in &pattern.attachments {
            if !att.description.is_empty() {
                text.push_str("\n\n");
                text.push_str(&att.description);
            }
        }
        match embed(&text, &config).await {
            Ok(embedding) => {
                indexed_patterns.push((pattern.clone(), embedding));
                if (i + 1) % 10 == 0 {
                    println!("  {}/{} embedded...", i + 1, total);
                }
            }
            Err(e) => {
                eprintln!("  ⚠️  {} — {}", pattern.name, e);
                errors += 1;
            }
        }
    }

    for (i, workflow) in workflows.iter().enumerate() {
        let text = format!(
            "{}: {}\n{}",
            workflow.name,
            workflow.description,
            workflow.content.as_text()
        );
        match embed(&text, &config).await {
            Ok(embedding) => {
                indexed_workflows.push((workflow.clone(), embedding));
                let idx = patterns.len() + i + 1;
                if idx % 10 == 0 {
                    println!("  {}/{} embedded...", idx, total);
                }
            }
            Err(e) => {
                eprintln!("  ⚠️  {} — {}", workflow.name, e);
                errors += 1;
            }
        }
    }

    let vector_store = VectorStore::open(&index_path, cfg.embedding.dimensions as i32).await?;
    vector_store
        .build_unified_index(&indexed_patterns, &indexed_workflows)
        .await?;

    println!(
        "✅ Indexed {} patterns + {} workflows ({} errors). Index: {}",
        indexed_patterns.len(),
        indexed_workflows.len(),
        errors,
        index_path.display()
    );

    Ok(())
}

/// Initialise the knowledge git repo and commit all existing patterns in one
/// bootstrap commit. After this, every `YamlStore::save` auto-commits via
/// the write path gate, and `mur pattern history` returns real history.
///
/// Also bootstraps the agents git repo at `~/.mur/agents/.git` if it exists
/// or if there are agent profiles to track.
pub(crate) fn cmd_reindex_bootstrap() -> Result<()> {
    use crate::store::versioned::VersionedYamlStore;
    use crate::store::versioned::agent::VersionedAgentStore;

    let mur_dir = default_mur_dir();

    // ── Knowledge layer ───────────────────────────────────────────────────────
    let already = mur_dir.join(".git").exists();
    let mut vs = if already {
        println!("Knowledge store already initialised — committing any un-tracked patterns.");
        VersionedYamlStore::open(&mur_dir)?
    } else {
        println!("Initialising knowledge store at {} ...", mur_dir.display());
        VersionedYamlStore::init(&mur_dir)?
    };

    let count = vs.bootstrap_all()?;
    if count == 0 {
        println!("Knowledge: all patterns already up to date.");
    } else {
        println!("Knowledge: {count} pattern(s) committed.");
    }

    // ── Agents layer ──────────────────────────────────────────────────────────
    let agents_dir = mur_dir.join("agents");
    if agents_dir.exists() {
        let agents_already = agents_dir.join(".git").exists();
        let mut avs = if agents_already {
            println!("Agents store already initialised — committing any un-tracked profiles.");
            VersionedAgentStore::open(&agents_dir)?
        } else {
            println!("Initialising agents store at {} ...", agents_dir.display());
            VersionedAgentStore::init(&agents_dir)?
        };

        let agent_count = avs.bootstrap_all()?;
        if agent_count == 0 {
            println!("Agents: all profiles already up to date.");
        } else {
            println!("Agents: {agent_count} agent profile(s) committed.");
        }
    }

    println!("Future saves are now auto-versioned.");
    println!("Try `mur pattern history <name>` or `mur agent history <name>`.");
    Ok(())
}
