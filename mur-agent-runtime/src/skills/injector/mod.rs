//! Layer-2 injection: which skills and memories reach the system prompt.
//!
//! HOW THE MEMORY TESTS IN THIS FILE WERE RUN: `cargo test -p
//! mur-agent-runtime` cannot build in the sandboxed dev environment — this
//! crate depends unconditionally on `whisper-rs` -> `whisper-rs-sys`, whose
//! build script shells out to `cmake`, which is not in the dev agent's spawn
//! allowlist. This module depends only on `mur_common`, so the tests in
//! `tests.rs` were verified by compiling THIS MODULE (via `#[path]`, not a copy) against
//! the real `mur-common` in an isolated harness, and each Required-memory
//! test was mutation-proven: reverting the partition turns all three red on
//! their own assertions. Re-verify with `cargo test -p mur-agent-runtime` in
//! CI, where cmake exists.

use mur_common::config::{MemoryConfig, SkillsConfig};
use mur_common::skill::TriggerKind;
use mur_common::skill::loader::LoadedSkill;
use mur_common::skill::types::{Category, HostId, Priority};
use std::collections::HashSet;

/// Heading of the Required-memory block. Pinned instructions outrank the
/// persona's defaults; saying so in the prompt is what lets the model resolve
/// a conflict (e.g. persona "mirror the user's language" vs a pinned
/// "always reply in zh-TW") in the user's favour.
pub(crate) const REQUIRED_HEADER: &str = "Permanent instructions (pinned by the user; \
     these OVERRIDE any conflicting default in your persona or style rules)";

fn priority_val(p: &Priority) -> u8 {
    match p {
        Priority::Low => 0,
        Priority::Normal => 1,
        Priority::High => 2,
        Priority::Critical => 3,
    }
}

#[derive(Debug, Clone, Default)]
pub struct InjectionResult {
    pub system_addendum: String,
    pub injected_names: Vec<String>,
    pub budget_skipped: bool,
}

// ponytail: 8 args, one over clippy's threshold. The honest fix is bundling
// `active_fleet`/`active_project`/`active_team` into one `Scope` struct — they
// always travel together — but that is mechanical churn across every call site
// and belongs in its own commit, not this bug fix.
#[allow(clippy::too_many_arguments)]
pub fn inject_layer2(
    skills: &[LoadedSkill],
    cfg: &SkillsConfig,
    mem: &MemoryConfig,
    context_fill_ratio: f64,
    recently_fired: &HashSet<String>,
    active_fleet: Option<&str>,
    active_project: Option<&str>,
    active_team: Option<&str>,
) -> InjectionResult {
    // Adaptive cutoff: when remaining context is too small, shed skills and
    // BestEffort memories — but NEVER Required ones. This used to be an early
    // `return`, placed before the Required split, so a long-lived runner
    // (whose `cumulative_input_tokens` never resets) silently lost every
    // permanent instruction once it crossed the threshold (plan invariant 2).
    let budget_skipped = cfg
        .adaptive
        .as_ref()
        .is_some_and(|ad| 1.0 - context_fill_ratio < ad.min_remaining_context_ratio);

    // Host + scope + not-on-demand. Split below into memories (always-on) and
    // trigger-gated skills.
    let visible: Vec<&LoadedSkill> = skills
        .iter()
        .filter(|s| {
            s.manifest.hosts.is_empty()
                || s.manifest
                    .hosts
                    .iter()
                    .any(|h| matches!(h, HostId::All | HostId::MurAgent))
        })
        // Scope: fleet/project-scoped skills inject only when the active scope
        // matches (fail-closed); user/enterprise always pass. active_project is
        // the member's cwd repo root; active_fleet is the turn's `fleet-<name>`
        // channel (membership-verified by the channel/delegate handler).
        .filter(|s| {
            mur_common::skill::manifest::scope_visible(
                s.manifest.scope,
                s.manifest.fleet.as_deref(),
                s.manifest.project.as_deref(),
                s.manifest.team.as_deref(),
                active_fleet,
                active_project,
                active_team,
            )
        })
        .filter(|s| s.manifest.visibility != mur_common::skill::manifest::Visibility::OnDemand)
        .collect();

    // Memories (`Category::Note`, written by the `remember` tool, `/remember`
    // and `mur notes create`) are always-on: the user stated them outright, so
    // they carry no `SessionStart` trigger and must not compete for skill
    // slots. Without this split they were written to disk and never reached a
    // prompt — the agent ignored its own saved rules and reported an empty
    // memory when asked.
    let all_notes: Vec<&LoadedSkill> = visible
        .iter()
        .copied()
        .filter(|s| s.manifest.category == Category::Note)
        .collect();

    // Required vs BestEffort (P1 plan §8). Partition BEFORE any ranking: a
    // permanent instruction must never be ranked, truncated, or ordered
    // against incidental memories, because then it survives by alphabetical
    // luck — the original bug. `lifecycle::injection_policy` is the single
    // reader of the policy tag; never match the tag inline here.
    //
    // Required means injection, NOT model compliance (plan invariant 4).
    let (required, mut notes): (Vec<&LoadedSkill>, Vec<&LoadedSkill>) =
        all_notes.into_iter().partition(|s| {
            mur_common::skill::lifecycle::injection_policy(&s.manifest)
                == Some(mur_common::skill::lifecycle::InjectionPolicy::Required)
        });

    // Only BestEffort competes for the capped slots (plan invariant 1).
    // ponytail: capped by max_in_prompt, same as skills; give notes
    // their own config knob only if a real memory list starves.
    notes.sort_by(|a, b| a.name.cmp(&b.name));
    let mut mem_dropped = notes.len().saturating_sub(mem.max_in_prompt);
    notes.truncate(mem.max_in_prompt);
    if budget_skipped {
        // Disclosed below as "N more not shown" — no silent caps.
        mem_dropped += notes.len();
        notes.clear();
    }

    // Filter: must have at least one `SessionStart` trigger.
    let mut candidates: Vec<&LoadedSkill> = visible
        .into_iter()
        .filter(|s| s.manifest.category != Category::Note)
        .filter(|s| {
            s.manifest
                .triggers
                .iter()
                .any(|t| matches!(t.kind, TriggerKind::SessionStart))
        })
        .collect();

    // Sort: trust desc, recent-fired boost, then priority asc, then name for determinism.
    candidates.sort_by(|a, b| {
        let trust_cmp = b.trust.cmp(&a.trust);
        if trust_cmp != std::cmp::Ordering::Equal {
            return trust_cmp;
        }
        let a_recent = recently_fired.contains(&a.name);
        let b_recent = recently_fired.contains(&b.name);
        if a_recent != b_recent {
            return b_recent.cmp(&a_recent);
        }
        priority_val(&a.manifest.priority)
            .cmp(&priority_val(&b.manifest.priority))
            .then(a.name.cmp(&b.name))
    });

    candidates.truncate(cfg.max_skills_in_prompt);
    if budget_skipped {
        candidates.clear();
    }

    // Adaptive token budget (char-based proxy).
    let budget = cfg
        .adaptive
        .as_ref()
        .map(|ad| {
            let remaining = 1.0 - context_fill_ratio;
            ((cfg.max_total_tokens as f64) * remaining.powf(ad.context_fill_decay)) as usize
        })
        .unwrap_or(cfg.max_total_tokens)
        .max(100);

    let mut spent = 0usize;
    let mut names = Vec::new();

    // Memories draw on their OWN character budget. Sharing the skill budget
    // would mean saving a memory silently evicts a bound skill.
    let mut mem_spent = 0usize;
    let mut mem_lines = Vec::new();

    let note_body = |s: &LoadedSkill| -> String {
        let body = s
            .manifest
            .content
            .note
            .as_deref()
            .unwrap_or(&s.manifest.content.r#abstract);
        format!("- {}: {}", s.name, body.trim())
    };

    // Required first and unconditionally: no top-K, no char cap, no sort.
    // Every Required memory is injected or none is (plan invariant 2) — the
    // `max_chars` `continue` below must never be reachable for Required, or a
    // permanent instruction would be silently dropped, which is exactly the
    // failure this split exists to remove.
    //
    // Required is a FIXED reservation: it is spent before BestEffort, so
    // BestEffort yields to Required and never the reverse (plan §5).
    let mut req_lines = Vec::new();
    for s in &required {
        let line = note_body(s);
        mem_spent += line.len() + 1;
        req_lines.push(line);
        names.push(s.name.clone());
    }

    for s in notes {
        let line = note_body(s);
        if mem_spent + line.len() + 1 > mem.max_chars {
            mem_dropped += 1;
            continue;
        }
        mem_spent += line.len() + 1;
        mem_lines.push(line);
        names.push(s.name.clone());
    }
    // No silent caps: a memory the user saved and cannot see in the prompt is
    // indistinguishable from one that was never saved.
    if mem_dropped > 0 {
        mem_lines.push(format!(
            "- ({mem_dropped} more memories not shown here — the user can list \
             them all with /memories)"
        ));
    }

    let mut lines = Vec::new();
    for s in candidates {
        let line = format!(
            "[Skill: {} ({:?})] {}",
            s.name,
            s.trust,
            s.manifest.content.r#abstract.trim()
        );
        if spent + line.len() + 1 > budget {
            continue;
        }
        spent += line.len() + 1;
        lines.push(line);
        names.push(s.name.clone());
    }
    if lines.is_empty() && mem_lines.is_empty() && req_lines.is_empty() {
        return InjectionResult {
            budget_skipped,
            ..Default::default()
        };
    }
    let mut system_addendum = String::new();
    if !lines.is_empty() {
        system_addendum.push_str(&format!(
            "\n--- Bound Skills ---\n{}\n---\n",
            lines.join("\n")
        ));
    }
    // Memory goes LAST, for two independent reasons that agree: standing user
    // instructions win the recency end of a long prompt, and a block that
    // changes when a memory is saved invalidates less of the cached prefix
    // sitting in front of it.
    if !mem_lines.is_empty() {
        system_addendum.push_str(&format!(
            "\n--- Memory (durable facts and rules you saved for this user; \
             /memories lists them, /forget <name> removes one) ---\n{}\n---\n",
            mem_lines.join("\n")
        ));
    }
    // Required gets its own block, after everything else: the user pinned
    // these, so they must outrank persona/style defaults (e.g. "mirror the
    // user's language") rather than read as one more remembered fact.
    if !req_lines.is_empty() {
        system_addendum.push_str(&format!(
            "\n--- {REQUIRED_HEADER} ---\n{}\n---\n",
            req_lines.join("\n")
        ));
    }
    InjectionResult {
        system_addendum,
        injected_names: names,
        budget_skipped,
    }
}

#[cfg(test)]
mod tests;
