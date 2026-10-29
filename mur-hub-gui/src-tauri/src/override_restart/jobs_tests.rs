use super::*;

#[test]
fn a_second_dialog_anywhere_is_refused() {
    let jobs = OverrideJobs::default();
    let _a = jobs.admit("a", "w1", JobState::Dialog).unwrap();
    assert_eq!(
        jobs.admit("b", "w2", JobState::Dialog).err(),
        Some(Busy::DialogOpen)
    );
}

#[test]
fn a_second_job_for_one_agent_is_refused() {
    let jobs = OverrideJobs::default();
    let _a = jobs.admit("a", "w1", JobState::Preparing).unwrap();
    assert_eq!(
        jobs.admit("a", "w1", JobState::Dialog).err(),
        Some(Busy::InProgress)
    );
    // Another agent is fine while nothing has a dialog open.
    assert!(jobs.admit("b", "w2", JobState::Preparing).is_ok());
}

#[test]
fn busy_reasons_are_the_audit_strings() {
    assert_eq!(Busy::DialogOpen.audit_reason(), "dialog open");
    assert_eq!(Busy::InProgress.audit_reason(), "job in progress");
}

#[test]
fn refuse_if_busy_names_the_agent() {
    let jobs = OverrideJobs::default();
    assert!(jobs.refuse_if_busy("a").is_ok());
    let _a = jobs.admit("a", "w", JobState::Preparing).unwrap();
    assert_eq!(
        jobs.refuse_if_busy("a").unwrap_err(),
        "an override restart of a is in progress"
    );
    assert!(jobs.refuse_if_busy("b").is_ok());
}

#[test]
fn entry_is_removed_on_drop_and_on_panic() {
    let jobs = OverrideJobs::default();
    drop(jobs.admit("a", "w", JobState::Preparing).unwrap());
    assert!(jobs.active_names().is_empty());

    let j = jobs.clone();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _e = j.admit("b", "w", JobState::Preparing).unwrap();
        panic!("boom");
    }));
    assert!(r.is_err());
    assert!(jobs.active_names().is_empty());
}

#[test]
fn publish_permit_commit_and_drop() {
    let jobs = OverrideJobs::default();
    let _e = jobs.admit("a", "w", JobState::Preparing).unwrap();
    let mut gate = jobs.gate("a");

    let permit = gate.begin_publish().expect("not quitting");
    assert_eq!(jobs.state_of("a"), Some(JobState::Publishing));
    drop(permit);
    assert_eq!(jobs.state_of("a"), Some(JobState::Preparing));

    gate.begin_publish().unwrap().commit("n");
    assert_eq!(
        jobs.state_of("a"),
        Some(JobState::Published { nonce: "n".into() })
    );
}

#[test]
fn quitting_refuses_the_publish() {
    let jobs = OverrideJobs::default();
    let _e = jobs.admit("a", "w", JobState::Preparing).unwrap();
    assert!(jobs.begin_quit().is_empty());
    assert!(jobs.is_quitting());
    assert!(jobs.gate("a").begin_publish().is_none());
}

#[test]
fn quitting_refuses_admission() {
    let jobs = OverrideJobs::default();
    jobs.begin_quit();
    assert_eq!(
        jobs.admit("a", "w", JobState::Dialog).err(),
        Some(Busy::InProgress)
    );
}

#[test]
fn quit_while_published_claims_once() {
    let jobs = OverrideJobs::default();
    let e = jobs.admit("a", "win", JobState::Preparing).unwrap();
    let mut gate = jobs.gate("a");
    gate.begin_publish().unwrap().commit("n1");

    let claims = jobs.begin_quit();
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].slug, "a");
    assert_eq!(claims[0].nonce, "n1");
    assert_eq!(claims[0].window, "win");
    assert_eq!(jobs.state_of("a"), Some(JobState::QuitWithdrawing));
    assert!(jobs.begin_quit().is_empty(), "claimed only once");

    assert!(!gate.begin_cleanup());
    drop(e);
    assert_eq!(jobs.active_names(), vec!["a".to_string()]);
    jobs.finish_claim("a");
    assert!(jobs.active_names().is_empty());
}

#[test]
fn quit_while_publishing_does_not_claim() {
    let jobs = OverrideJobs::default();
    let _e = jobs.admit("a", "w", JobState::Preparing).unwrap();
    let mut gate = jobs.gate("a");
    let permit = gate.begin_publish().unwrap();
    assert!(jobs.begin_quit().is_empty());
    permit.commit("n");
    assert_eq!(
        jobs.state_of("a"),
        Some(JobState::Published { nonce: "n".into() })
    );
    assert!(gate.begin_cleanup());
    assert_eq!(jobs.state_of("a"), Some(JobState::Cleaning));
}

#[test]
fn quit_while_cleaning_does_not_claim() {
    let jobs = OverrideJobs::default();
    let _e = jobs.admit("a", "w", JobState::Preparing).unwrap();
    let mut gate = jobs.gate("a");
    gate.begin_publish().unwrap().commit("n");
    assert!(gate.begin_cleanup());
    assert!(jobs.begin_quit().is_empty());
    assert_eq!(jobs.state_of("a"), Some(JobState::Cleaning));
}

#[test]
fn finish_claim_leaves_a_non_claimed_entry() {
    let jobs = OverrideJobs::default();
    let _e = jobs.admit("a", "w", JobState::Preparing).unwrap();
    jobs.finish_claim("a");
    assert_eq!(jobs.active_names(), vec!["a".to_string()]);
}

#[test]
fn a_poisoned_mutex_still_works() {
    let jobs = OverrideJobs::default();
    jobs.poison();
    assert!(jobs.admit("a", "w", JobState::Dialog).is_ok());
}

#[test]
fn quit_prompt_opens_once() {
    let jobs = OverrideJobs::default();
    assert!(jobs.begin_quit_prompt());
    assert!(!jobs.begin_quit_prompt());
    jobs.end_quit_prompt();
    assert!(jobs.begin_quit_prompt());
}

#[test]
fn set_state_moves_an_entry() {
    let jobs = OverrideJobs::default();
    let _e = jobs.admit("a", "w", JobState::Dialog).unwrap();
    jobs.set_state("a", JobState::Preparing);
    assert_eq!(jobs.state_of("a"), Some(JobState::Preparing));
    // The dialog slot is free again.
    assert!(jobs.admit("b", "w", JobState::Dialog).is_ok());
}
