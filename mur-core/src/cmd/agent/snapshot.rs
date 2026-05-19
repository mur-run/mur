use anyhow::Result;
use mur_common::agent::AgentProfile;

pub fn cmd_snapshot_pull(name: &str, dry_run: bool) -> Result<()> {
    let profile_path = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".mur/agents")
        .join(name)
        .join("profile.yaml");
    let filter = if profile_path.exists() {
        let body = std::fs::read_to_string(&profile_path)?;
        let profile: AgentProfile = serde_yaml_ng::from_str(&body)?;
        profile.federation.filter
    } else {
        mur_common::agent::PatternFilter::default()
    };

    if dry_run {
        let store = crate::store::yaml::YamlStore::default_store()?;
        let all = store.list_all()?;
        let matched = crate::federation::snapshot::apply_filter(all, &filter);
        println!(
            "Would snapshot {} patterns for agent '{name}':",
            matched.len()
        );
        for p in &matched {
            println!("  {} (importance={:.2})", p.name, p.importance);
        }
        return Ok(());
    }

    let snap = crate::federation::pull_snapshot(name, &filter)?;
    println!(
        "Snapshot pulled for '{name}': {} @ {}",
        snap.knowledge_commit, snap.taken_at
    );
    Ok(())
}

pub fn cmd_snapshot_show(name: &str) -> Result<()> {
    match crate::federation::read_snapshot_ref(name)? {
        Some(r) => {
            println!("commit:   {}", r.knowledge_commit);
            println!("taken_at: {}", r.taken_at);
            println!(
                "filter:   tier={:?} maturity={:?} importance_min={} max_count={}",
                r.filter.tier, r.filter.maturity, r.filter.importance_min, r.filter.max_count
            );
        }
        None => println!("No snapshot for agent '{name}'"),
    }
    Ok(())
}
