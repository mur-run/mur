//! `/remember` `/memories` `/forget` — the TUI surface of the memory
//! behavioral layer (federation P2b). Pure functions that return display
//! strings; the slash dispatch just prints them. Writes stay AGENT-LOCAL:
//! destroying shared knowledge belongs to `mur notes`, not a chat pane.

use anyhow::{Context, Result, bail};
use std::path::Path;

use mur_common::skill::lifecycle::{InjectionPolicy, NoteKind, injection_policy, note_kind};
use mur_common::skill::loader::{SkillScope, load_all};
use mur_common::skill::note::{NoteSpec, note_manifest};
use mur_common::skill::stats::{LifecycleState, SkillStats};
use mur_common::skill::store::agent_skill_dir;

mod create;
mod pending;

pub use create::*;
pub use pending::*;

/// One visible memory, resolved far enough to display and to budget.
///
/// `rendered` is [`mur_compress::memory_budget::render_for_injection`] — the
/// prompt form, not bare content — so the tokens shown in `/memories` are the
/// tokens actually spent (plan invariant 5).
pub struct ListedMemory {
    pub name: String,
    pub kind: NoteKind,
    pub policy: InjectionPolicy,
    pub scope_label: &'static str,
    pub state: LifecycleState,
    pub description: String,
    pub rendered: String,
    /// Agent-local memories are the only ones a chat pane may rewrite;
    /// shared and federated notes belong to `mur notes`.
    pub agent_local: bool,
}

/// Every non-forgotten note this agent can see, in load order.
///
/// One definition behind `/memories`, the pre-send budget gate, and the
/// promote/demote commands: three readers that must never disagree about
/// which memories are Required or how big they are.
pub fn list_memories(home: &Path, agent: &str) -> Vec<ListedMemory> {
    let cache_root = home.join("agents").join(agent).join("knowledge_cache");
    let mut out = Vec::new();
    for s in load_all(home, agent) {
        let Some(kind) = note_kind(&s.manifest) else {
            continue;
        };
        let (scope_label, stats_path) = match s.scope {
            SkillScope::Agent => ("agent", SkillStats::path_agent(home, agent, &s.name)),
            SkillScope::Global if s.dir.starts_with(&cache_root) => {
                ("federated", SkillStats::path(home, &s.name))
            }
            SkillScope::Global => ("shared", SkillStats::path(home, &s.name)),
        };
        let state = SkillStats::load(&stats_path)
            .ok()
            .flatten()
            .map(|st| st.lifecycle_state)
            .unwrap_or(LifecycleState::Draft);
        if state == LifecycleState::Destroyed {
            continue; // forgotten — stays invisible here too
        }
        let body = s
            .manifest
            .content
            .note
            .as_deref()
            .unwrap_or(&s.manifest.content.r#abstract);
        out.push(ListedMemory {
            rendered: mur_compress::memory_budget::render_for_injection(&s.name, body),
            // `injection_policy` is the single reader of the policy tag.
            policy: injection_policy(&s.manifest).unwrap_or(InjectionPolicy::BestEffort),
            name: s.name,
            kind,
            scope_label,
            state,
            description: s.manifest.description,
            agent_local: s.scope == SkillScope::Agent,
        });
    }
    out
}

/// The Required set, rendered for the budget layer in list order.
///
/// Order is preserved and never sorted: Required bypasses ranking entirely
/// (plan invariant 1).
pub fn rendered_required(
    listed: &[ListedMemory],
) -> Vec<mur_compress::memory_budget::RenderedRequiredMemory> {
    listed
        .iter()
        .filter(|m| m.policy == InjectionPolicy::Required)
        .map(|m| {
            mur_compress::memory_budget::RenderedRequiredMemory::new(
                m.name.clone(),
                m.rendered.clone(),
            )
        })
        .collect()
}

/// The Required-budget send gate, shared by every path that can start a turn.
///
/// Returns the blocked-send overlay when the permanent instructions do not fit
/// their fixed reservation, `None` otherwise. Plan invariant 2 is "every
/// Required memory is injected or none is", and the injector honours that by
/// injecting Required unconditionally — so the *only* place that can uphold the
/// budget is the send path. It therefore cannot live in the TUI alone: `--plain`
/// and `mur agent send` reach the same runtime and would otherwise sail past a
/// reservation the interactive user is blocked on.
pub fn required_budget_block(home: &Path, agent: &str) -> Option<String> {
    let listed = list_memories(home, agent);
    let usage = mur_compress::memory_ux::usage_summary(&rendered_required(&listed));
    mur_compress::memory_block::blocked_send_overlay_if_blocked(&usage)
}

/// `1234` → `1,234`. The plan writes budget figures as `4,320 / 3,500`, and a
/// four-digit token count is much easier to misread without the separator.
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `/memories` — two sections with two distinct creation entries (plan §11).
///
/// The split is the whole point: the system never guesses the contract level,
/// so "permanent instruction" and "remembered information" are separate lists
/// with separate ways to create them. Required carries a usage meter because
/// it draws on a fixed reservation; BestEffort does not, because it competes
/// for whatever is left.
///
/// Wording is fixed by plan §4 and must not drift into the banned parallel
/// vocabulary (hard / pinned / guaranteed / always). "Always added" describes
/// INJECTION, not compliance (invariant 4).
pub fn memories(home: &Path, agent: &str) -> String {
    let listed = list_memories(home, agent);
    if listed.is_empty() {
        return "no memories yet — /instruct <text> for a permanent instruction, \
                /remember <text> for something to recall when relevant"
            .into();
    }

    let row = |m: &ListedMemory| {
        format!(
            "  {:<28} {:<5} {:<9} {:<10} {}",
            m.name,
            kind_str(m.kind),
            m.scope_label,
            format!("{:?}", m.state),
            m.description
        )
    };

    let usage = mur_compress::memory_ux::usage_summary(&rendered_required(&listed));
    let mut out = String::new();

    // ── Permanent instructions ──
    out.push_str(&format!(
        "Permanent instructions — always added to the AI's context ({} / {} tokens)\n",
        thousands(usage.used_tokens),
        thousands(usage.budget_tokens)
    ));
    let required: Vec<&ListedMemory> = listed
        .iter()
        .filter(|m| m.policy == InjectionPolicy::Required)
        .collect();
    if required.is_empty() {
        out.push_str("  (none) — /instruct <text> to add one\n");
    } else {
        // Per-item token sizes come from the same projection as the meter, so
        // the parts always sum to the whole the user is shown.
        let sizes: std::collections::HashMap<&str, usize> = usage
            .per_item
            .iter()
            .map(|i| (i.memory_id.as_str(), i.rendered_tokens))
            .collect();
        for m in &required {
            let size = sizes.get(m.name.as_str()).copied().unwrap_or(0);
            out.push_str(&format!("{} [~{} tokens]\n", row(m), thousands(size)));
        }
        out.push_str(
            "  per item: /instruct-edit <name> <text> · /unpin <name> (remember only when \
             relevant) · /forget <name>\n",
        );
    }
    if usage.over_budget {
        // The block is stated here too, not only at send time: a user opening
        // /memories after a blocked send must see why, and one who has not yet
        // pressed Enter deserves the warning before they lose a turn.
        out.push_str(&format!(
            "  ⚠ over budget — sends are blocked until you reduce by at least ~{} tokens\n",
            thousands(usage.reduce_by_tokens)
        ));
    }
    if usage.count_warning {
        out.push_str(&format!(
            "  note: {} permanent instructions — a long list is harder to keep coherent \
             (this never blocks)\n",
            required.len()
        ));
    }

    // ── Remembered information ──
    out.push_str("\nRemembered information — may be used when relevant\n");
    let best_effort: Vec<&ListedMemory> = listed
        .iter()
        .filter(|m| m.policy == InjectionPolicy::BestEffort)
        .collect();
    if best_effort.is_empty() {
        out.push_str("  (none) — /remember <text> to add one\n");
    } else {
        for m in &best_effort {
            out.push_str(&format!("{}\n", row(m)));
        }
        out.push_str("  per item: /pin <name> (make permanent) · /forget <name>\n");
    }

    out.push_str("\n(agent-local · federated · shared)");
    out
}

/// Agent-local notes that are still injectable, newest first.
///
/// One definition for two callers: `/forget last` resolves to `[0]`, and the
/// completion menu offers the whole list. A Destroyed note is excluded from
/// both — offering a name that `forget` would then reject is worse than
/// offering nothing.
pub fn live_note_names(home: &Path, agent: &str) -> Vec<String> {
    let mut live: Vec<_> = load_all(home, agent)
        .into_iter()
        .filter(|s| s.scope == SkillScope::Agent && note_kind(&s.manifest).is_some())
        .filter(|s| {
            SkillStats::load(&SkillStats::path_agent(home, agent, &s.name))
                .ok()
                .flatten()
                .is_none_or(|st| st.lifecycle_state != LifecycleState::Destroyed)
        })
        .collect();
    live.sort_by_key(|s| std::cmp::Reverse(s.manifest.updated_at));
    live.into_iter().map(|s| s.name).collect()
}

/// `/forget <name|last>` — delete an AGENT-LOCAL memory.
///
/// Deleting a **permanent instruction** requires explicit confirmation (plan
/// §7): the user asked for that guarantee on purpose, and `/forget last` in
/// particular could otherwise destroy one with a single reflex keystroke.
/// Remembered information deletes straight away, as it always has.
pub fn forget(home: &Path, agent: &str, target: Option<&str>) -> Result<MemoryOutcome> {
    let target = target.ok_or_else(|| anyhow::anyhow!("usage: /forget <name|last>"))?;
    let name = if target == "last" {
        live_note_names(home, agent)
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no agent-local memories to forget"))?
    } else {
        target.to_string()
    };

    let is_required = list_memories(home, agent)
        .iter()
        .any(|m| m.name == name && m.policy == InjectionPolicy::Required);
    if is_required {
        return Ok(MemoryOutcome::Confirm {
            prompt: format!(
                "Delete the permanent instruction '{name}'?\n\
                 This removes the text for good. To stop it being added every turn but keep \
                 it, cancel and use /unpin {name} instead."
            ),
            pending: PendingMemoryOp {
                kind: PendingKind::Delete,
                name,
            },
        });
    }
    destroy_note(home, agent, &name).map(MemoryOutcome::Done)
}

fn kind_str(k: NoteKind) -> &'static str {
    match k {
        NoteKind::Rule => "rule",
        NoteKind::Fact => "fact",
    }
}

#[cfg(test)]
mod tests;
