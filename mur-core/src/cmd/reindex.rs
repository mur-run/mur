use anyhow::Result;

use crate::store::default_mur_dir;
use crate::store::workflow_yaml::WorkflowYamlStore;

pub(crate) async fn cmd_reindex() -> Result<()> {
    use crate::store::embedding::{EmbeddingConfig, embed_batch};
    use crate::store::vector::LanceDbStore as VectorStore;

    // Rebuild the Channel SQLite read-model from the event-log manifests.
    {
        let home = crate::paths::mur_root(None);
        let store = mur_channel::ChannelStore::new(&home);
        let index = mur_channel::index::ChannelIndex::open(&home)?;
        let n = index.rebuild_from(&store)?;
        println!("✅ rebuilt channel index: {n} channel(s)");
    }

    let workflow_store = WorkflowYamlStore::default_store()?;
    let workflows = workflow_store.list_all()?;

    if workflows.is_empty() {
        println!("No workflows to index.");
        return Ok(());
    }

    let cfg = crate::store::config::load_config()?;
    let config = EmbeddingConfig::from_config(&cfg);
    let index_path = mur_common::home::mur_home().join("index");

    println!(
        "🔄 Reindexing {} workflows using {} ({})...",
        workflows.len(),
        config.model,
        match &config.provider {
            crate::store::embedding::EmbeddingProvider::Ollama { base_url } => base_url.clone(),
            crate::store::embedding::EmbeddingProvider::OpenAI { .. } => "OpenAI".into(),
        }
    );

    // Collect all texts first
    let mut texts: Vec<String> = Vec::with_capacity(workflows.len());
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
    let mut indexed_workflows = Vec::new();
    for (i, workflow) in workflows.iter().enumerate() {
        if let Some(emb) = embeddings[i].take() {
            indexed_workflows.push((workflow.clone(), emb));
        }
    }

    let vector_store = VectorStore::open(&index_path, cfg.embedding.dimensions as i32).await?;
    vector_store.build_unified_index(&indexed_workflows).await?;

    println!(
        "✅ Indexed {} workflows ({} errors). Index: {}",
        indexed_workflows.len(),
        errors,
        index_path.display()
    );

    Ok(())
}

/// Initialise the agents git repo at `~/.mur/agents/.git` and commit all
/// existing agent profiles in one bootstrap commit. After this, every profile
/// save auto-commits and `mur agent history` returns real history.
///
/// The knowledge-layer bootstrap (patterns) was retired with the Pattern store.
pub(crate) fn cmd_reindex_bootstrap() -> Result<()> {
    use crate::store::versioned::agent::VersionedAgentStore;

    let mur_dir = default_mur_dir();
    let agents_dir = mur_dir.join("agents");
    if !agents_dir.exists() {
        println!(
            "No agents directory at {} — nothing to bootstrap.",
            agents_dir.display()
        );
        return Ok(());
    }

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

    println!("Future saves are now auto-versioned. Try `mur agent history <name>`.");
    Ok(())
}
