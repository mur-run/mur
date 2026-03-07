use anyhow::Result;
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::*;
use std::io::{self, Write};

use crate::evolve;
use crate::store::workflow_yaml::WorkflowYamlStore;
use crate::store::yaml::YamlStore;

pub(crate) fn cmd_workflow_list() -> Result<()> {
    let store = WorkflowYamlStore::default_store()?;
    let workflows = store.list_all()?;

    if workflows.is_empty() {
        println!("No workflows found. Create one with `mur workflow new`.");
        return Ok(());
    }

    println!("📋 Workflows ({}):\n", workflows.len());
    for w in &workflows {
        let steps = w.steps.len();
        println!("  {} — {} ({} steps)", w.name, w.description, steps);
    }

    Ok(())
}

pub(crate) fn cmd_workflow_show(name: &str) -> Result<()> {
    let store = WorkflowYamlStore::default_store()?;
    let w = store.get(name)?;

    println!("📋 Workflow: {}\n", w.name);
    println!("Description: {}", w.description);
    println!("Content: {}", w.content.as_text());

    if !w.steps.is_empty() {
        println!("\nSteps:");
        for step in &w.steps {
            print!("  {}. {}", step.order, step.description);
            if let Some(cmd) = &step.command {
                print!(" (`{}`)", cmd);
            }
            println!();
        }
    }

    if !w.tools.is_empty() {
        println!("\nTools: {}", w.tools.join(", "));
    }

    if !w.trigger.is_empty() {
        println!("Trigger: {}", w.trigger);
    }

    Ok(())
}

pub(crate) fn cmd_workflow_new() -> Result<()> {
    use mur_common::workflow::{FailureAction, Step};

    let store = WorkflowYamlStore::default_store()?;

    print!("Workflow name (kebab-case): ");
    io::stdout().flush()?;
    let mut name = String::new();
    io::stdin().read_line(&mut name)?;
    let name = name.trim().to_string();

    if name.is_empty() {
        println!("Name cannot be empty.");
        return Ok(());
    }
    if store.exists(&name) {
        println!("Workflow '{}' already exists.", name);
        return Ok(());
    }

    print!("Description: ");
    io::stdout().flush()?;
    let mut desc = String::new();
    io::stdin().read_line(&mut desc)?;
    let desc = desc.trim().to_string();

    print!("Trigger (when to use, e.g. 'when deploying to production'): ");
    io::stdout().flush()?;
    let mut trigger = String::new();
    io::stdin().read_line(&mut trigger)?;
    let trigger = trigger.trim().to_string();

    println!("Steps (enter description, empty line to finish):");
    let mut steps = Vec::new();
    let mut order = 1u32;
    loop {
        print!("  Step {}: ", order);
        io::stdout().flush()?;
        let mut step_desc = String::new();
        io::stdin().read_line(&mut step_desc)?;
        let step_desc = step_desc.trim().to_string();
        if step_desc.is_empty() {
            break;
        }

        print!("    Command (optional): ");
        io::stdout().flush()?;
        let mut cmd = String::new();
        io::stdin().read_line(&mut cmd)?;
        let cmd = cmd.trim().to_string();

        steps.push(Step {
            order,
            description: step_desc,
            command: if cmd.is_empty() { None } else { Some(cmd) },
            tool: None,
            needs_approval: false,
            on_failure: FailureAction::Abort,
        });
        order += 1;
    }

    let workflow = mur_common::workflow::Workflow {
        base: KnowledgeBase {
            name: name.clone(),
            description: desc,
            content: Content::Plain(trigger.clone()),
            ..Default::default()
        },
        steps,
        variables: vec![],
        source_sessions: vec![],
        trigger,
        tools: vec![],
        published_version: 0,
        permission: Default::default(),
    };

    store.save(&workflow)?;
    println!("Created workflow: {}", name);
    Ok(())
}

pub(crate) fn cmd_suggest(create: bool) -> Result<()> {
    use evolve::compose::suggest_workflows_with_patterns;
    use evolve::cooccurrence::CooccurrenceMatrix;
    use evolve::decompose::{analyze_workflow_for_extraction, extract_pattern_from_step};

    let pattern_store = YamlStore::default_store()?;
    let workflow_store = WorkflowYamlStore::default_store()?;
    let patterns = pattern_store.list_all()?;
    let workflows = workflow_store.list_all()?;

    // ─── Part 1: Workflow composition from co-occurrence ─────────────

    let matrix_path = CooccurrenceMatrix::default_path();
    let matrix = CooccurrenceMatrix::load(&matrix_path)?;

    println!("🔗 Knowledge ↔ Workflow Intelligence\n");
    println!("── Co-occurrence Data ──");
    println!("  Tracked pairs: {}", matrix.pair_count());

    let suggestions = suggest_workflows_with_patterns(&matrix, 5, &patterns);

    if suggestions.is_empty() {
        println!("  No workflow composition suggestions yet.");
        println!("  (Need 3+ patterns co-occurring 5+ times)");
    } else {
        println!("\n── Workflow Composition Suggestions ──\n");
        for (i, s) in suggestions.iter().enumerate() {
            println!(
                "  {}. {} (score: {})",
                i + 1,
                s.suggested_name,
                s.cooccurrence_score,
            );
            println!("     Patterns: {}", s.patterns.join(", "));
            println!("     Trigger: {}", s.suggested_trigger);

            if create {
                if workflow_store.exists(&s.suggested_name) {
                    println!(
                        "     -> Workflow '{}' already exists, skipping.",
                        s.suggested_name
                    );
                } else {
                    // Create a draft workflow from the suggestion
                    let wf = mur_common::workflow::Workflow {
                        base: KnowledgeBase {
                            name: s.suggested_name.clone(),
                            description: format!(
                                "Auto-suggested workflow from {} co-occurring patterns",
                                s.patterns.len()
                            ),
                            content: Content::Plain(format!(
                                "Combines patterns: {}",
                                s.patterns.join(", ")
                            )),
                            tags: collect_tags_from_patterns(&s.patterns, &patterns),
                            ..Default::default()
                        },
                        steps: vec![],
                        variables: vec![],
                        source_sessions: vec![],
                        trigger: s.suggested_trigger.clone(),
                        tools: vec![],
                        published_version: 0,
                        permission: Default::default(),
                    };
                    workflow_store.save(&wf)?;
                    println!("     -> Created draft workflow: {}", s.suggested_name);

                    // Add cross-reference: link each source pattern to this workflow
                    for pname in &s.patterns {
                        if let Ok(mut p) = pattern_store.get(pname)
                            && !p.links.workflows.contains(&s.suggested_name)
                        {
                            p.base.links.workflows.push(s.suggested_name.clone());
                            let _ = pattern_store.save(&p);
                        }
                    }
                }
            }
            println!();
        }
    }

    // ─── Part 2: Workflow decomposition into patterns ────────────────

    if !workflows.is_empty() {
        println!("── Decomposition Candidates ──\n");

        let mut any_candidates = false;
        for wf in &workflows {
            let candidates = analyze_workflow_for_extraction(wf, &patterns);
            if candidates.is_empty() {
                continue;
            }
            any_candidates = true;

            println!("  Workflow: {} ({} candidates)", wf.name, candidates.len());
            for c in &candidates {
                println!("    Step {}: \"{}\"", c.step_index + 1, c.step_description,);
                println!("      -> Pattern: {}", c.suggested_pattern_name);
                println!("      Reason: {}", c.reason);

                if create {
                    if pattern_store.exists(&c.suggested_pattern_name) {
                        println!(
                            "      -> Pattern '{}' already exists, skipping.",
                            c.suggested_pattern_name
                        );
                    } else if let Some(pattern) = extract_pattern_from_step(wf, c.step_index) {
                        pattern_store.save(&pattern)?;
                        println!(
                            "      -> Created draft pattern: {}",
                            c.suggested_pattern_name
                        );
                    }
                }
            }
            println!();
        }

        if !any_candidates {
            println!("  No decomposition candidates found in existing workflows.");
        }
    }

    // ─── Summary ─────────────────────────────────────────────────────

    if !create && (!suggestions.is_empty() || !workflows.is_empty()) {
        println!("Run `mur suggest --create` to auto-create suggested items as drafts.");
    }

    Ok(())
}

/// Collect tags from a set of pattern names.
pub(crate) fn collect_tags_from_patterns(
    names: &[String],
    patterns: &[Pattern],
) -> mur_common::pattern::Tags {
    let mut topics: Vec<String> = Vec::new();
    let mut languages: Vec<String> = Vec::new();

    for name in names {
        if let Some(p) = patterns.iter().find(|p| &p.name == name) {
            for t in &p.tags.topics {
                if !topics.contains(t) {
                    topics.push(t.clone());
                }
            }
            for l in &p.tags.languages {
                if !languages.contains(l) {
                    languages.push(l.clone());
                }
            }
        }
    }

    mur_common::pattern::Tags {
        topics,
        languages,
        extra: Default::default(),
    }
}
