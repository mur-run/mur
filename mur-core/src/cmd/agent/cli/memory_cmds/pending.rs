//! Confirmed destructive operations: `/pin`, `/unpin`, `/forget` pending state.

use super::*;

/// What a memory command wants the UI to do next.
///
/// Confirmation is modeled as data rather than a blocking prompt so the
/// decision logic stays pure and testable: the command decides *whether* a
/// confirmation is owed, the UI decides how to ask.
#[derive(Debug)]
pub enum MemoryOutcome {
    /// Completed; show this message.
    Done(String),
    /// Nothing has changed yet. Ask, then call [`apply_pending`] on Confirm
    /// and simply drop `pending` on Cancel (acceptance test 15).
    Confirm {
        prompt: String,
        pending: PendingMemoryOp,
    },
}

/// A destructive memory operation awaiting explicit confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMemoryOp {
    pub kind: PendingKind,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingKind {
    /// Delete a permanent instruction.
    Delete,
    /// Demote a permanent instruction to remembered information.
    Demote,
    /// Save an edit that deliberately overflows the budget (plan §7's
    /// "Save anyway"). Carries the new body, so confirming applies exactly the
    /// text that was projected — re-reading the composer later could apply
    /// something the user never saw a budget figure for.
    EditAnyway { body: String },
}

/// `/unpin <name>` — demote a permanent instruction to remembered information
/// ("Remember only when relevant").
///
/// Always confirmed (plan §7): demotion drops an injection guarantee the user
/// deliberately asked for, and it must never happen by accident. Nothing —
/// overflow, inactivity, recency, importance, a classifier — may auto-demote,
/// so this is the only path and it always goes through a human.
pub fn unpin(home: &Path, agent: &str, target: Option<&str>) -> Result<MemoryOutcome> {
    let name = target
        .ok_or_else(|| anyhow::anyhow!("usage: /unpin <name>"))?
        .to_string();
    let listed = list_memories(home, agent);
    let m = listed
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| anyhow::anyhow!("no memory named '{name}'"))?;
    if m.policy != InjectionPolicy::Required {
        bail!("'{name}' is already remembered information, not a permanent instruction");
    }
    require_agent_local(m)?;
    Ok(MemoryOutcome::Confirm {
        prompt: format!(
            "Make '{name}' remembered information instead of a permanent instruction?\n\
             It will then be used only when it seems relevant, not added every turn.\n\
             The text is kept. Confirm to change, Cancel to keep it permanent."
        ),
        pending: PendingMemoryOp {
            kind: PendingKind::Demote,
            name,
        },
    })
}

/// `/pin <name>` — promote remembered information to a permanent instruction
/// ("Make permanent").
///
/// Rejected over budget and the memory **stays BestEffort** (plan §7,
/// acceptance test 8). Promotion needs no confirmation: it adds a guarantee
/// rather than removing one, and it is reversible with `/unpin`.
pub fn pin(home: &Path, agent: &str, target: Option<&str>) -> Result<String> {
    use mur_compress::memory_budget::{
        RequiredBudgetProjection, WriteDecision, WriteOperation, canonical_memory_counter,
        exceeds_per_memory_char_limit,
    };

    let name = target.ok_or_else(|| anyhow::anyhow!("usage: /pin <name>"))?;
    let listed = list_memories(home, agent);
    let m = listed
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| anyhow::anyhow!("no memory named '{name}'"))?;
    if m.policy == InjectionPolicy::Required {
        bail!("'{name}' is already a permanent instruction");
    }
    require_agent_local(m)?;

    let body = note_body_of(home, agent, name)?;
    if exceeds_per_memory_char_limit(&body) {
        bail!(
            "'{name}' is {} characters, over the {}-character limit for a single permanent \
             instruction — split it first",
            body.chars().count(),
            mur_compress::memory_budget::MAX_REQUIRED_MEMORY_CHARS
        );
    }

    let current = rendered_required(&listed);
    let delta = canonical_memory_counter().count(&m.rendered) as isize;
    let projection = RequiredBudgetProjection::from_rendered(&current, delta);
    let decision = projection.decide(WriteOperation::Promote);

    if decision == WriteDecision::Reject {
        bail!(
            "permanent instructions are full: this would need {} of {} tokens.\n\
             '{name}' is unchanged and stays remembered information. Free about ~{} tokens \
             first — /memories lists each instruction with its size.",
            thousands(projection.projected_tokens),
            thousands(projection.budget_tokens),
            thousands(projection.projected_tokens - projection.budget_tokens),
        );
    }

    set_policy_on_disk(home, agent, name, InjectionPolicy::Required)?;
    let mut msg = format!(
        "📌 '{name}' is now a permanent instruction — added to the AI's context every turn \
         (presence, not compliance). /unpin {name} to undo."
    );
    if decision == WriteDecision::AllowWithWarning {
        msg.push_str(&format!(
            "\n⚠ using {} of {} tokens — close to full.",
            thousands(projection.projected_tokens),
            thousands(projection.budget_tokens)
        ));
    }
    Ok(msg)
}

/// Carry out a confirmed destructive operation. Called only after the user
/// confirms; on Cancel the caller drops the [`PendingMemoryOp`] and nothing
/// has been touched.
pub fn apply_pending(home: &Path, agent: &str, op: &PendingMemoryOp) -> Result<String> {
    // Matched by reference: `PendingKind::EditAnyway` carries the new body, so
    // this is no longer a `Copy` enum.
    match &op.kind {
        PendingKind::Demote => {
            set_policy_on_disk(home, agent, &op.name, InjectionPolicy::BestEffort)?;
            let listed = list_memories(home, agent);
            let usage = mur_compress::memory_ux::usage_summary(&rendered_required(&listed));
            let mut msg = format!(
                "'{}' is now remembered information — used when relevant, not every turn.",
                op.name
            );
            // Recovery is the point of demotion (acceptance test 10), so say
            // plainly when it has cleared the block.
            msg.push_str(&format!(
                "\nPermanent instructions: {} / {} tokens.",
                thousands(usage.used_tokens),
                thousands(usage.budget_tokens)
            ));
            if !usage.over_budget {
                msg.push_str(" Within budget — sends are no longer blocked.");
            } else {
                msg.push_str(&format!(
                    " Still over by ~{} tokens.",
                    thousands(usage.reduce_by_tokens)
                ));
            }
            Ok(msg)
        }
        PendingKind::Delete => destroy_note(home, agent, &op.name),
        PendingKind::EditAnyway { body } => {
            set_note_body_on_disk(home, agent, &op.name, body)?;
            let listed = list_memories(home, agent);
            let usage = mur_compress::memory_ux::usage_summary(&rendered_required(&listed));
            let mut msg = format!("✏️ updated permanent instruction '{}'.", op.name);
            msg.push_str(&format!(
                "\nPermanent instructions: {} / {} tokens.",
                thousands(usage.used_tokens),
                thousands(usage.budget_tokens)
            ));
            if usage.over_budget {
                // Say the consequence plainly — the user chose this state, but
                // must not be surprised by the next refused send.
                msg.push_str(&format!(
                    " Over budget: sends are blocked until you reduce by at least ~{} tokens.",
                    thousands(usage.reduce_by_tokens)
                ));
            }
            Ok(msg)
        }
    }
}

/// Body of an agent-local note, read back from disk.
pub(super) fn note_body_of(home: &Path, agent: &str, name: &str) -> Result<String> {
    let listed = list_memories(home, agent);
    let m = listed
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| anyhow::anyhow!("no memory named '{name}'"))?;
    // `rendered` is `- {name}: {body}`; recover the body rather than re-reading
    // the manifest, so this cannot disagree with what was budgeted.
    Ok(m.rendered
        .strip_prefix(&format!("- {name}: "))
        .unwrap_or(&m.rendered)
        .to_string())
}

/// A chat pane may only rewrite AGENT-LOCAL memories; shared and federated
/// notes belong to `mur notes`.
pub(super) fn require_agent_local(m: &ListedMemory) -> Result<()> {
    if !m.agent_local {
        bail!(
            "'{}' is a {} note — change it with `mur notes`, not from a chat pane",
            m.name,
            m.scope_label
        );
    }
    Ok(())
}

/// Rewrite a note's policy tag in place, preserving everything else.
pub(super) fn set_policy_on_disk(
    home: &Path,
    agent: &str,
    name: &str,
    policy: InjectionPolicy,
) -> Result<()> {
    let dir = agent_skill_dir(home, agent).join(name);
    let path = dir.join("skill.yaml");
    let mut manifest: mur_common::skill::SkillManifest =
        serde_yaml::from_str(&std::fs::read_to_string(&path).with_context(|| {
            format!("no agent-local memory named '{name}' (shared notes: use `mur notes`)")
        })?)
        .with_context(|| format!("parse {}", path.display()))?;
    mur_common::skill::lifecycle::set_injection_policy(&mut manifest, policy);
    manifest.updated_at = chrono::Utc::now();
    mur_common::skill::validate(&manifest).context("invalid note")?;
    mur_common::skill::store::write_to_dir(&dir, &manifest)
        .map_err(|e| anyhow::anyhow!("write note: {e}"))?;
    Ok(())
}

/// Rewrite a note's body (and its derived description) in place, preserving
/// its policy, kind, and everything else.
///
/// Mirrors [`set_policy_on_disk`]: the manifest is read, one field group is
/// changed, and it is written back through the same validate + store path, so
/// an edit cannot produce a note that a fresh write would have rejected.
pub(super) fn set_note_body_on_disk(
    home: &Path,
    agent: &str,
    name: &str,
    body: &str,
) -> Result<()> {
    let dir = agent_skill_dir(home, agent).join(name);
    let path = dir.join("skill.yaml");
    let mut manifest: mur_common::skill::SkillManifest =
        serde_yaml::from_str(&std::fs::read_to_string(&path).with_context(|| {
            format!("no agent-local memory named '{name}' (shared notes: use `mur notes`)")
        })?)
        .with_context(|| format!("parse {}", path.display()))?;
    // `list_memories` reads `content.note` and falls back to the abstract. Keep
    // the same shape `note_manifest` creates: `note` is the body, while the
    // abstract and description carry the one-line summary. Writing the full
    // body into the abstract here would make an edited note structurally
    // different from a freshly created one.
    let description: String = body.chars().take(60).collect();
    manifest.content.note = Some(body.to_string());
    manifest.content.r#abstract = description.clone();
    manifest.description = description;
    manifest.updated_at = chrono::Utc::now();
    mur_common::skill::validate(&manifest).context("invalid note")?;
    mur_common::skill::store::write_to_dir(&dir, &manifest)
        .map_err(|e| anyhow::anyhow!("write note: {e}"))?;
    Ok(())
}

/// Mark a note Destroyed, removing it from injection everywhere.
pub(super) fn destroy_note(home: &Path, agent: &str, name: &str) -> Result<String> {
    let stats_path = SkillStats::path_agent(home, agent, name);
    let mut stats = SkillStats::load(&stats_path)?.ok_or_else(|| {
        anyhow::anyhow!("no agent-local memory named '{name}' (shared notes: use `mur notes`)")
    })?;
    stats.lifecycle_state = LifecycleState::Destroyed;
    stats.lifecycle_changed_at = chrono::Utc::now();
    std::fs::write(&stats_path, serde_json::to_string(&stats)?)?;
    Ok(format!("forgot '{name}' — it will no longer be injected"))
}
