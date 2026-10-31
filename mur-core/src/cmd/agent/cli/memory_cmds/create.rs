//! `/remember` and `/instruct` — note creation and in-place edit.

use super::*;

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
pub(super) fn parse_kind_and_body(args: &[String], usage: &str) -> Result<(NoteKind, String)> {
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
pub(super) fn note_name() -> String {
    format!("note-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"))
}

pub(super) fn write_note(
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
pub(super) fn write_note_named(
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
