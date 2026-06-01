use anyhow::Result;

use crate::store::workflow_yaml::WorkflowYamlStore;
use crate::store::yaml::{YamlStore, default_mur_dir};

pub(crate) async fn cmd_reindex() -> Result<()> {
    use crate::store::embedding::{EmbeddingConfig, embed_batch};
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

    // Collect all texts first
    let mut texts: Vec<String> = Vec::with_capacity(patterns.len() + workflows.len());

    for pattern in &patterns {
        let mut text = format!(
            "{}: {}\n{}",
            pattern.name,
            pattern.description,
            pattern.content.as_text()
        );
        for att in &pattern.attachments {
            if !att.description.is_empty() {
                text.push_str("\n\n");
                text.push_str(&att.description);
            }
        }
        texts.push(text);
    }
    for workflow in &workflows {
        let text = format!(
            "{}: {}\n{}",
            workflow.name,
            workflow.description,
            workflow.content.as_text()
        );
        texts.push(text);
    }

    let total = texts.len();
    let mut embeddings: Vec<Option<Vec<f32>>> = vec![None; total];
    let mut errors = 0;

    let embed_batch_size = if config.batch_size > 0 {
        config.batch_size
    } else {
        32 // fallback
    };
    for batch_start in (0..total).step_by(embed_batch_size) {
        let batch_end = (batch_start + embed_batch_size).min(total);
        let batch: Vec<String> = texts[batch_start..batch_end].to_vec();
        match embed_batch(&batch, &config).await {
            Ok(batch_embs) => {
                for (j, emb) in batch_embs.into_iter().enumerate() {
                    embeddings[batch_start + j] = Some(emb);
                }
                println!("  {}/{} embedded...", batch_end, total);
            }
            Err(e) => {
                eprintln!(
                    "  ⚠️  batch {}-{} embedding failed: {}",
                    batch_start, batch_end, e
                );
                errors += batch_end - batch_start;
            }
        }
    }

    // Pair results back
    let mut indexed_patterns = Vec::new();
    let mut indexed_workflows = Vec::new();

    for (i, pattern) in patterns.iter().enumerate() {
        if let Some(emb) = embeddings[i].take() {
            indexed_patterns.push((pattern.clone(), emb));
        }
    }
    for (i, workflow) in workflows.iter().enumerate() {
        if let Some(emb) = embeddings[patterns.len() + i].take() {
            indexed_workflows.push((workflow.clone(), emb));
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
