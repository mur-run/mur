//! M7b — Skill recombination engine.
//!
//! Two parent skills produce a third under one of three strategies:
//! Union (superset merge), Intersection (overlap merge), LLM (delegated).
//! Output strictly on the invoking agent — peer state is never written.

pub mod llm;
pub mod peer_ref;
pub mod strategy;

pub use strategy::{FitnessCtx, RecombineStrategy};

use anyhow::{Result, anyhow, bail};
use chrono::Utc;
use mur_common::skill::evolution::EvolutionEvent;
use mur_common::skill::gene::SkillGene;
use mur_common::skill::hash::content_sha256;
use mur_common::skill::stats::LifecycleState;
use mur_common::skill::manifest::SkillManifest;
use mur_common::skill::stats::SkillStats;
use mur_common::skill::validate::validate;
use std::path::{Path, PathBuf};

use peer_ref::{LoadedSkillRef, SkillRef, load_skill_ref};

#[derive(Debug, Clone)]
pub struct RecombineOptions {
    pub a_ref: SkillRef,
    pub b_ref: SkillRef,
    pub strategy: RecombineStrategy,
    pub output_name: Option<String>,
    pub dry_run: bool,
    pub current_agent: String,
}

#[derive(Debug)]
pub struct RecombineOutcome {
    pub manifest: SkillManifest,
    pub manifest_yaml: String,
    pub written_to: Option<PathBuf>,
    pub evolution_event_appended: bool,
    pub output_name: String,
    pub strategy: RecombineStrategy,
}

pub async fn run_recombine(home: &Path, opts: &RecombineOptions) -> Result<RecombineOutcome> {
    let a = load_skill_ref(home, &opts.current_agent, &opts.a_ref)?;
    let b = load_skill_ref(home, &opts.current_agent, &opts.b_ref)?;

    let output_name = opts
        .output_name
        .clone()
        .unwrap_or_else(|| format!("{}-x-{}", a.manifest.name, b.manifest.name));

    // Name collision: refuse before any work.
    let output_path = home
        .join("agents")
        .join(&opts.current_agent)
        .join("skills")
        .join(&output_name);
    if !opts.dry_run && output_path.exists() {
        bail!(
            "skill '{output_name}' already exists on agent '{}'; pass --name to choose another",
            opts.current_agent
        );
    }

    let manifest = match opts.strategy {
        RecombineStrategy::Union => union_or_intersection(&a, &b, true)?,
        RecombineStrategy::Intersection => union_or_intersection(&a, &b, false)?,
        RecombineStrategy::Llm => {
            llm::llm_recombine(home, &a.manifest, &b.manifest, &output_name)
                .await
                .map_err(|e| anyhow!("LLM strategy failed: {e}"))?
        }
    };

    // For Union/Intersection we synthesised a SkillGene-derived manifest; for
    // LLM the model already produced one. In both cases, set authoritative
    // fields the caller chose (name, version reset, evolution metadata).
    let manifest = finalize_manifest(manifest, &output_name, &a, &b, opts.strategy)?;

    // Schema validate
    validate(&manifest).map_err(|e| anyhow!("recombined manifest failed validation: {e:?}"))?;

    let manifest_yaml = serde_yaml_ng::to_string(&manifest)?;

    if opts.dry_run {
        return Ok(RecombineOutcome {
            manifest,
            manifest_yaml,
            written_to: None,
            evolution_event_appended: false,
            output_name,
            strategy: opts.strategy,
        });
    }

    // Atomic write: temp + rename
    std::fs::create_dir_all(&output_path)?;
    let final_path = output_path.join("skill.yaml");
    let tmp_path = output_path.join("skill.yaml.tmp");
    std::fs::write(&tmp_path, &manifest_yaml)?;
    std::fs::rename(&tmp_path, &final_path)?;

    // Stats sidecar at Draft lifecycle
    let digest = content_sha256(&manifest).unwrap_or_default();
    let mut stats = SkillStats::new(&output_name, &manifest.version, &digest, Utc::now());
    stats.lifecycle_state = LifecycleState::Draft;
    peer_ref::write_initial_stats(home, &opts.current_agent, &output_name, &stats)?;

    Ok(RecombineOutcome {
        manifest,
        manifest_yaml,
        written_to: Some(final_path),
        evolution_event_appended: true,
        output_name,
        strategy: opts.strategy,
    })
}

fn union_or_intersection(
    a: &LoadedSkillRef,
    b: &LoadedSkillRef,
    is_union: bool,
) -> Result<SkillManifest> {
    let ga = SkillGene::from_manifest(&a.manifest)
        .map_err(|e| anyhow!("parent A ({}): {e}", a.ref_.display()))?;
    let gb = SkillGene::from_manifest(&b.manifest)
        .map_err(|e| anyhow!("parent B ({}): {e}", b.ref_.display()))?;

    let merged_gene = if is_union {
        strategy::union(&ga, &gb).map_err(|e| anyhow!("{e}"))?
    } else {
        let fit = FitnessCtx {
            a_agent: a.agent_label.clone(),
            b_agent: b.agent_label.clone(),
            a_success_rate: success_rate(&a.stats),
            b_success_rate: success_rate(&b.stats),
            a_weight: 0.5,
            b_weight: 0.5,
        };
        strategy::intersection(&ga, &gb, &fit).map_err(|e| anyhow!("{e}"))?
    };

    // Rebuild the manifest from the merged gene, copying static fields from
    // parent A (description, abstract, category are not "genes" in M7b).
    let mut out = a.manifest.clone();
    out.content.procedure = Some(merged_gene.to_procedure());
    out.triggers = merged_gene.to_triggers();
    out.requires = merged_gene.to_requirements();
    out.mcp_requirements = merged_gene.to_mcp_requirements();
    Ok(out)
}

fn finalize_manifest(
    mut m: SkillManifest,
    output_name: &str,
    a: &LoadedSkillRef,
    b: &LoadedSkillRef,
    strategy: RecombineStrategy,
) -> Result<SkillManifest> {
    m.name = output_name.to_string();
    m.version = "0.1.0".to_string();
    m.publisher = "agent:recombiner".to_string();

    // Generation = max(parent_generation) + 1
    let max_gen = m
        .evolution_log
        .iter()
        .chain(a.manifest.evolution_log.iter())
        .chain(b.manifest.evolution_log.iter())
        .map(|e| e.generation)
        .max()
        .unwrap_or(0);
    let next_gen = max_gen.saturating_add(1);

    // Reset evolution log to a single Recombined event (this is a new skill).
    m.evolution_log = vec![EvolutionEvent::recombined(
        &m.version,
        next_gen,
        &a.ref_.display(),
        &b.ref_.display(),
        strategy.as_str(),
        output_name,
    )];

    // Reset transfer_chain — the offspring originates here.
    m.transfer_chain = vec![];

    Ok(m)
}

fn success_rate(s: &SkillStats) -> f64 {
    let denom = s.success_count + s.failure_count;
    if denom == 0 {
        0.0
    } else {
        s.success_count as f64 / denom as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::stats::SkillStats;
    use tempfile::TempDir;

    fn write_skill(home: &Path, agent: &str, name: &str, yaml: &str) {
        let dir = home.join("agents").join(agent).join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("skill.yaml"), yaml).unwrap();
    }

    fn minimal_yaml(name: &str, trigger: &str, intent: &str, desc: &str) -> String {
        format!(
            r#"name: {name}
version: 0.1.0
publisher: human:test
description: test skill
category: workflow
content:
  abstract: a
  procedure:
    steps:
      - description: {desc}
        intent: {intent}
triggers:
  - type: command
    pattern: "{trigger}"
priority: normal
"#
        )
    }

    #[tokio::test]
    async fn dry_run_does_not_write_output_skill() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        write_skill(home, "self", "a", &minimal_yaml("a", "/a", "i1", "do A"));
        write_skill(home, "self", "b", &minimal_yaml("b", "/b", "i2", "do B"));

        let opts = RecombineOptions {
            a_ref: peer_ref::parse_ref("a").unwrap(),
            b_ref: peer_ref::parse_ref("b").unwrap(),
            strategy: RecombineStrategy::Union,
            output_name: Some("a-x-b".into()),
            dry_run: true,
            current_agent: "self".into(),
        };
        let outcome = run_recombine(home, &opts).await.unwrap();
        assert!(outcome.written_to.is_none());
        assert!(!outcome.evolution_event_appended);
        let out_path = home.join("agents/self/skills/a-x-b/skill.yaml");
        assert!(!out_path.exists());
    }

    #[tokio::test]
    async fn apply_writes_manifest_and_stats() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        write_skill(home, "self", "a", &minimal_yaml("a", "/a", "i1", "do A"));
        write_skill(home, "self", "b", &minimal_yaml("b", "/b", "i2", "do B"));

        let opts = RecombineOptions {
            a_ref: peer_ref::parse_ref("a").unwrap(),
            b_ref: peer_ref::parse_ref("b").unwrap(),
            strategy: RecombineStrategy::Union,
            output_name: Some("merged".into()),
            dry_run: false,
            current_agent: "self".into(),
        };
        let outcome = run_recombine(home, &opts).await.unwrap();
        assert!(outcome.written_to.is_some());
        assert!(outcome.evolution_event_appended);
        let out_path = home.join("agents/self/skills/merged/skill.yaml");
        assert!(out_path.exists());
        let stats_path = SkillStats::path_agent(home, "self", "merged");
        let stats = SkillStats::load(&stats_path).unwrap().unwrap();
        assert!(matches!(stats.lifecycle_state, LifecycleState::Draft));
    }

    #[tokio::test]
    async fn name_collision_errors() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        write_skill(home, "self", "a", &minimal_yaml("a", "/a", "i1", "do A"));
        write_skill(home, "self", "b", &minimal_yaml("b", "/b", "i2", "do B"));
        write_skill(home, "self", "merged", &minimal_yaml("merged", "/m", "im", "exists"));

        let opts = RecombineOptions {
            a_ref: peer_ref::parse_ref("a").unwrap(),
            b_ref: peer_ref::parse_ref("b").unwrap(),
            strategy: RecombineStrategy::Union,
            output_name: Some("merged".into()),
            dry_run: false,
            current_agent: "self".into(),
        };
        let err = run_recombine(home, &opts).await.unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }
}
