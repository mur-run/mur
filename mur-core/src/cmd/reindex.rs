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
pub(crate) fn cmd_reindex_bootstrap() -> Result<()> {
    use crate::store::versioned::VersionedYamlStore;

    let mur_dir = default_mur_dir();
    let already = mur_dir.join(".git").exists();

    let mut vs = if already {
        println!("Versioned store already initialised — committing any un-tracked patterns.");
        VersionedYamlStore::open(&mur_dir)?
    } else {
        println!("Initialising versioned store at {} ...", mur_dir.display());
        VersionedYamlStore::init(&mur_dir)?
    };

    let count = vs.bootstrap_all()?;
    if count == 0 {
        println!("All patterns already up to date in versioned store.");
    } else {
        println!("Bootstrap complete: {count} pattern(s) committed.");
        println!("Future saves are now auto-versioned. Try `mur pattern history <name>`.");
    }
    Ok(())
}
