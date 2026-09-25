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

/// `/remember [--kind rule|fact] <text…>` — save an agent-local Draft note as
/// **remembered information** (BestEffort).
///
/// One of the two creation entries (plan §11). This one never produces a
/// permanent instruction, no matter how the text reads: "永遠用中文" typed here
/// is BestEffort, because only the explicit `/instruct` entry may create
/// Required (invariant 3). The user typed it themselves, so there is no
/// confirmation dance and the provenance is human.
pub fn remember(home: &Path, agent: &str, args: &[String]) -> Result<String> {
    let (kind, body) = parse_kind_and_body(args, "/remember")?;
    let (name, description) = write_note(home, agent, &body, kind, InjectionPolicy::BestEffort)?;
    Ok(format!(
        "📝 remembered ({}, Draft, agent-local): {description} — /pin {name} to make it \
         permanent, /forget {name} to undo",
        kind_str(kind)
    ))
}

/// `/instruct [--kind rule|fact] <text…>` — save an agent-local note as a
/// **permanent instruction** (Required).
///
/// The second creation entry (plan §11), deliberately separate from
/// `/remember` so the contract level is always the user's explicit choice.
///
/// Governed by the write-time projection (plan §7): under 80% it just saves,
/// in the 80–100% band it saves with a warning, and over budget it is
/// **rejected** — there is no "Add anyway", because a Required set that does
/// not fit blocks every subsequent turn.
pub fn instruct(home: &Path, agent: &str, args: &[String]) -> Result<String> {
    use mur_compress::memory_budget::{
        RequiredBudgetProjection, WriteDecision, WriteOperation, canonical_memory_counter,
        exceeds_per_memory_char_limit, render_for_injection,
    };

    let (kind, body) = parse_kind_and_body(args, "/instruct")?;

    // Per-memory char limit first: this is a validation error on THIS
    // instruction, independent of how much budget is free, so it must not be
    // reported as a budget problem.
    if exceeds_per_memory_char_limit(&body) {
        bail!(
            "this instruction is {} characters, over the {}-character limit for a single \
             permanent instruction — split it into focused instructions",
            body.chars().count(),
            mur_compress::memory_budget::MAX_REQUIRED_MEMORY_CHARS
        );
    }

    // Project BEFORE writing anything: a rejected add must not create the
    // memory (acceptance test 7).
    let listed = list_memories(home, agent);
    let current = rendered_required(&listed);
    // Name is not known until write time, and the wrapper is `- {name}: {body}`
    // — so project with the name this write will actually use.
    let name = note_name();
    let delta = canonical_memory_counter().count(&render_for_injection(&name, &body)) as isize;
    let projection = RequiredBudgetProjection::from_rendered(&current, delta);

    match projection.decide(WriteOperation::Add) {
        WriteDecision::Reject => {
            bail!(
                "permanent instructions are full: this would need {} of {} tokens.\n\
                 Nothing was saved. Free about ~{} tokens first — /memories lists each \
                 instruction with its size, and /unpin <name> or /forget <name> makes room.\n\
                 (Saved as remembered information instead? /remember {})",
                thousands(projection.projected_tokens),
                thousands(projection.budget_tokens),
                thousands(projection.projected_tokens - projection.budget_tokens),
                body.chars().take(40).collect::<String>().trim_end(),
            )
        }
        decision => {
            let (name, description) =
                write_note_named(home, agent, &name, &body, kind, InjectionPolicy::Required)?;
            let mut msg = format!(
                "📌 permanent instruction saved ({}, agent-local): {description}\n\
                 It is added to the AI's context every turn — that guarantees it is PRESENT, \
                 not that the model always obeys it.\n\
                 /unpin {name} to keep it only when relevant · /forget {name} to delete",
                kind_str(kind)
            );
            if decision == WriteDecision::AllowWithWarning {
                msg.push_str(&format!(
                    "\n⚠ using {} of {} tokens — close to full.",
                    thousands(projection.projected_tokens),
                    thousands(projection.budget_tokens)
                ));
            }
            let required_count = current.len() + 1;
            if mur_compress::memory_budget::required_count_warning(required_count) {
                msg.push_str(&format!(
                    "\nnote: {required_count} permanent instructions — a long list is harder \
                     to keep coherent (this never blocks)."
                ));
            }
            Ok(msg)
        }
    }
}

/// Shared `--kind rule|fact` + free-text parsing for both creation entries, so
/// the two cannot drift in how they read their arguments.
fn parse_kind_and_body(args: &[String], usage: &str) -> Result<(NoteKind, String)> {
    let mut kind = NoteKind::Fact;
    let mut words: Vec<&str> = Vec::new();
    let mut it = args.iter().map(String::as_str);
    while let Some(w) = it.next() {
        if w == "--kind" {
            match it.next() {
                Some("rule") => kind = NoteKind::Rule,
                Some("fact") => kind = NoteKind::Fact,
                other => bail!("--kind expects rule|fact, got {other:?}"),
            }
        } else {
            words.push(w);
        }
    }
    let body = words.join(" ");
    if body.trim().is_empty() {
        bail!("usage: {usage} [--kind rule|fact] <text>");
    }
    Ok((kind, body))
}

/// Timestamped name: collision-free enough for a human-driven command.
// ponytail: no slug generation — meaningful names come from agents or
// `mur notes create`; rename later if it matters.
fn note_name() -> String {
    format!("note-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"))
}

fn write_note(
    home: &Path,
    agent: &str,
    body: &str,
    kind: NoteKind,
    policy: InjectionPolicy,
) -> Result<(String, String)> {
    write_note_named(home, agent, &note_name(), body, kind, policy)
}

/// Write an agent-local Draft note at `name` with an explicit injection
/// policy.
///
/// The ONE write path behind both creation entries. `NoteSpec` has no policy
/// field on purpose, so the policy is applied through
/// `lifecycle::set_injection_policy` — the single writer of the tag.
fn write_note_named(
    home: &Path,
    agent: &str,
    name: &str,
    body: &str,
    kind: NoteKind,
    policy: InjectionPolicy,
) -> Result<(String, String)> {
    let description: String = body.chars().take(60).collect();
    let dir = agent_skill_dir(home, agent).join(name);
    if dir.join("skill.yaml").exists() {
        bail!("memory '{name}' already exists — try again in a second");
    }
    let mut manifest = note_manifest(&NoteSpec {
        name,
        description: &description,
        body,
        kind,
        publisher: "human:local",
    });
    mur_common::skill::lifecycle::set_injection_policy(&mut manifest, policy);
    mur_common::skill::validate(&manifest).context("invalid note")?;
    mur_common::skill::store::write_to_dir(&dir, &manifest)
        .map_err(|e| anyhow::anyhow!("write note: {e}"))?;
    let stats = SkillStats::new(name, "1.0.0", "", chrono::Utc::now());
    std::fs::write(
        SkillStats::path_agent(home, agent, name),
        serde_json::to_string(&stats)?,
    )?;
    Ok((name.to_string(), description))
}

/// `/instruct-edit <name> <text…>` — rewrite a permanent instruction's body.
///
/// The ONLY operation that may deliberately create overflow state (plan §7).
/// Add and promote are rejected over budget; edit merely **warns** and allows
/// "Save anyway", because a user whose Required set is already too big needs
/// to be able to merge and rewrite instructions to get out. Locking editing
/// at exactly the moment it is most needed would trap them with no legal move.
///
/// Saving into overflow immediately enters the blocking state: the next Send
/// is refused until the set fits again. The returned message says so rather
/// than letting the user discover it by losing a turn.
pub fn instruct_edit(home: &Path, agent: &str, args: &[String]) -> Result<MemoryOutcome> {
    use mur_compress::memory_budget::{
        RequiredBudgetProjection, WriteDecision, WriteOperation, canonical_memory_counter,
        exceeds_per_memory_char_limit, render_for_injection,
    };

    let (name, rest) = args
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("usage: /instruct-edit <name> <text>"))?;
    let body = rest.join(" ");
    if body.trim().is_empty() {
        bail!("usage: /instruct-edit <name> <text>");
    }

    let listed = list_memories(home, agent);
    let m = listed
        .iter()
        .find(|m| &m.name == name)
        .ok_or_else(|| anyhow::anyhow!("no memory named '{name}'"))?;
    if m.policy != InjectionPolicy::Required {
        bail!(
            "'{name}' is remembered information — edit applies to permanent instructions; \
             /pin {name} first if you meant to make it one"
        );
    }
    require_agent_local(m)?;

    // A per-memory validation error on THIS instruction, independent of how
    // much budget is free — so it is never reported as a budget problem.
    if exceeds_per_memory_char_limit(&body) {
        bail!(
            "this instruction is {} characters, over the {}-character limit for a single \
             permanent instruction — split it into focused instructions",
            body.chars().count(),
            mur_compress::memory_budget::MAX_REQUIRED_MEMORY_CHARS
        );
    }

    // Delta is new minus old: an edit that shrinks an instruction frees budget,
    // and must be projected as a negative rather than as a fresh add.
    let counter = canonical_memory_counter();
    let old_tokens = counter.count(&m.rendered) as isize;
    let new_tokens = counter.count(&render_for_injection(name, &body)) as isize;
    let current = rendered_required(&listed);
    let projection = RequiredBudgetProjection::from_rendered(&current, new_tokens - old_tokens);

    // Edit never rejects (§7), so Reject is unreachable here — but routing
    // through the same table keeps the thresholds in one place.
    let decision = projection.decide(WriteOperation::Edit);
    if projection.is_over_budget() {
        // Over budget: confirm rather than silently entering the blocking
        // state. The user gets to choose, and is told exactly what follows.
        return Ok(MemoryOutcome::Confirm {
            prompt: format!(
                "Save '{name}' anyway?\n\
                 Permanent instructions would use {} of {} tokens — over the limit, so sends \
                 will be blocked until you reduce by at least ~{} tokens.\n\
                 Confirm to save anyway, Cancel to leave it unchanged.",
                thousands(projection.projected_tokens),
                thousands(projection.budget_tokens),
                thousands(projection.projected_tokens - projection.budget_tokens),
            ),
            pending: PendingMemoryOp {
                kind: PendingKind::EditAnyway { body },
                name: name.clone(),
            },
        });
    }

    set_note_body_on_disk(home, agent, name, &body)?;
    let mut msg = format!("✏️ updated permanent instruction '{name}'.");
    if decision == WriteDecision::AllowWithWarning {
        msg.push_str(&format!(
            "\n⚠ using {} of {} tokens — close to full.",
            thousands(projection.projected_tokens),
            thousands(projection.budget_tokens)
        ));
    }
    Ok(MemoryOutcome::Done(msg))
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
fn note_body_of(home: &Path, agent: &str, name: &str) -> Result<String> {
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
fn require_agent_local(m: &ListedMemory) -> Result<()> {
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
fn set_policy_on_disk(
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
fn set_note_body_on_disk(home: &Path, agent: &str, name: &str, body: &str) -> Result<()> {
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
fn destroy_note(home: &Path, agent: &str, name: &str) -> Result<String> {
    let stats_path = SkillStats::path_agent(home, agent, name);
    let mut stats = SkillStats::load(&stats_path)?.ok_or_else(|| {
        anyhow::anyhow!("no agent-local memory named '{name}' (shared notes: use `mur notes`)")
    })?;
    stats.lifecycle_state = LifecycleState::Destroyed;
    stats.lifecycle_changed_at = chrono::Utc::now();
    std::fs::write(&stats_path, serde_json::to_string(&stats)?)?;
    Ok(format!("forgot '{name}' — it will no longer be injected"))
}

fn kind_str(k: NoteKind) -> &'static str {
    match k {
        NoteKind::Rule => "rule",
        NoteKind::Fact => "fact",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_memories_forget_cycle() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();

        let msg = remember(
            home,
            "a1",
            &[
                "--kind".into(),
                "rule".into(),
                "reply".into(),
                "in".into(),
                "zh-TW".into(),
            ],
        )
        .unwrap();
        assert!(msg.contains("rule") && msg.contains("/forget"));

        let listing = memories(home, "a1");
        assert!(listing.contains("reply in zh-TW"));
        assert!(listing.contains("agent"));

        // BestEffort, so it deletes without a confirmation step.
        let gone = match forget(home, "a1", Some("last")).unwrap() {
            MemoryOutcome::Done(msg) => msg,
            MemoryOutcome::Confirm { .. } => {
                panic!("remembered information must delete without confirmation")
            }
        };
        assert!(gone.contains("forgot"));
        assert!(
            !memories(home, "a1").contains("reply in zh-TW"),
            "forgotten note must disappear from /memories"
        );
    }

    #[test]
    fn forget_refuses_shared_notes_and_empty_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        assert!(forget(home, "a1", None).is_err());
        // a GLOBAL note exists but no agent-local one: 'last' finds nothing,
        // and naming it directly reports the agent-local miss.
        let dir = home.join("skills/shared-note");
        let m = note_manifest(&NoteSpec {
            name: "shared-note",
            description: "d",
            body: "b",
            kind: NoteKind::Fact,
            publisher: "human:t",
        });
        mur_common::skill::store::write_to_dir(&dir, &m).unwrap();
        assert!(forget(home, "a1", Some("last")).is_err());
        let err = forget(home, "a1", Some("shared-note"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("mur notes"),
            "must point at the right tool: {err}"
        );
    }

    #[test]
    fn remember_rejects_empty_and_bad_kind() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(remember(tmp.path(), "a1", &[]).is_err());
        assert!(
            remember(
                tmp.path(),
                "a1",
                &["--kind".into(), "opinion".into(), "x".into()]
            )
            .is_err()
        );
    }

    /// The menu and `/forget last` must see the same set, in the same order:
    /// a forgotten note stays out of both, and the newest is first.
    #[test]
    fn live_note_names_drops_forgotten_and_leads_with_the_newest() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        remember(h, "a", &["first".to_string()]).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        remember(h, "a", &["second".to_string()]).unwrap();

        let names = live_note_names(h, "a");
        assert_eq!(names.len(), 2, "{names:?}");

        forget(h, "a", Some("last")).unwrap();
        let after = live_note_names(h, "a");
        assert_eq!(
            after.len(),
            1,
            "a forgotten note is still listed: {after:?}"
        );
        assert_eq!(
            after[0], names[1],
            "`last` must forget the newest, leaving the older one"
        );
    }

    /// Plan §7 / acceptance test 15: deleting a **permanent instruction** asks
    /// first, and Cancel is a genuine no-op — the memory is still there and
    /// still Required.
    #[test]
    fn deleting_a_permanent_instruction_confirms_and_cancel_changes_nothing() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        instruct(h, "a", &["reply".into(), "in".into(), "zh-TW".into()]).unwrap();
        let name = list_memories(h, "a")[0].name.clone();

        let outcome = forget(h, "a", Some(&name)).unwrap();
        let pending = match outcome {
            MemoryOutcome::Confirm { pending, .. } => pending,
            MemoryOutcome::Done(_) => {
                panic!("a permanent instruction must never delete unconfirmed")
            }
        };
        assert_eq!(pending.kind, PendingKind::Delete);

        // Cancel = drop the pending op. Nothing was applied.
        let still = list_memories(h, "a");
        assert_eq!(still.len(), 1, "cancel must not delete");
        assert_eq!(still[0].policy, InjectionPolicy::Required);

        // Confirm applies it.
        apply_pending(h, "a", &pending).unwrap();
        assert!(
            list_memories(h, "a").is_empty(),
            "confirming must delete the instruction"
        );
    }

    /// Plan §7: demotion is always confirmed, and confirming keeps the TEXT
    /// while dropping only the injection guarantee.
    #[test]
    fn demotion_confirms_and_keeps_the_text() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        instruct(h, "a", &["always".into(), "use".into(), "tabs".into()]).unwrap();
        let name = list_memories(h, "a")[0].name.clone();

        let pending = match unpin(h, "a", Some(&name)).unwrap() {
            MemoryOutcome::Confirm { pending, .. } => pending,
            MemoryOutcome::Done(_) => panic!("demotion must be confirmed"),
        };
        assert_eq!(pending.kind, PendingKind::Demote);
        assert_eq!(
            list_memories(h, "a")[0].policy,
            InjectionPolicy::Required,
            "cancel must leave it permanent"
        );

        apply_pending(h, "a", &pending).unwrap();
        let after = list_memories(h, "a");
        assert_eq!(after.len(), 1, "demotion must not delete the memory");
        assert_eq!(after[0].policy, InjectionPolicy::BestEffort);
        assert!(
            after[0].rendered.contains("always use tabs"),
            "the text must survive demotion: {}",
            after[0].rendered
        );
    }

    /// Plan §11 / acceptance test 13: the same text is Required through
    /// `/instruct` and BestEffort through `/remember`. No reclassification —
    /// the creation path alone decides, however the content reads.
    #[test]
    fn the_creation_path_alone_decides_the_policy() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        // Reads exactly like a standing order, but /remember never makes one.
        remember(h, "a", &["永遠用中文回答我".into()]).unwrap();
        assert_eq!(
            list_memories(h, "a")[0].policy,
            InjectionPolicy::BestEffort,
            "/remember must never create a permanent instruction"
        );

        let home2 = tempfile::tempdir().unwrap();
        instruct(home2.path(), "a", &["永遠用中文回答我".into()]).unwrap();
        assert_eq!(
            list_memories(home2.path(), "a")[0].policy,
            InjectionPolicy::Required,
            "/instruct must create a permanent instruction"
        );
    }

    /// `/memories` shows both sections with their own creation entry, so the
    /// user is never left guessing which command makes which kind (plan §11).
    #[test]
    fn memories_lists_both_sections_with_both_creation_entries() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        instruct(h, "a", &["speak".into(), "plainly".into()]).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        remember(h, "a", &["david".into(), "uses".into(), "zsh".into()]).unwrap();

        let out = memories(h, "a");
        assert!(
            out.contains("Permanent instructions") && out.contains("Remembered information"),
            "both sections must be present:\n{out}"
        );
        assert!(
            out.contains("/instruct-edit") && out.contains("/pin"),
            "each section must offer its own actions:\n{out}"
        );
        // The meter reports the budget, not a guess.
        assert!(
            out.contains(&thousands(mur_compress::memory_budget::REQUIRED_BUDGET_TOKENS)),
            "the usage meter must show the fixed budget:\n{out}"
        );
    }

    /// Editing an instruction rewrites the body that gets injected, rather than
    /// leaving the old text in place behind a new description.
    #[test]
    fn editing_an_instruction_rewrites_the_injected_body() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        instruct(h, "a", &["use".into(), "tabs".into()]).unwrap();
        let name = list_memories(h, "a")[0].name.clone();

        let msg = match instruct_edit(h, "a", &[name.clone(), "use".into(), "spaces".into()])
            .unwrap()
        {
            MemoryOutcome::Done(m) => m,
            MemoryOutcome::Confirm { .. } => {
                panic!("a small edit is within budget and must not need confirmation")
            }
        };
        assert!(msg.contains("updated"), "{msg}");

        let after = list_memories(h, "a");
        assert!(
            after[0].rendered.contains("use spaces") && !after[0].rendered.contains("use tabs"),
            "the injected text must be the edited one: {}",
            after[0].rendered
        );
        assert_eq!(
            after[0].policy,
            InjectionPolicy::Required,
            "an edit must not change the policy"
        );
    }
}
