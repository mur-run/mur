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
        out.contains(&thousands(
            mur_compress::memory_budget::REQUIRED_BUDGET_TOKENS
        )),
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

    let msg = match instruct_edit(h, "a", &[name.clone(), "use".into(), "spaces".into()]).unwrap() {
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
