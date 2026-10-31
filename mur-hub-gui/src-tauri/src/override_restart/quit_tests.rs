//! `run_claims` against a temp home. The live file is written in the
//! writer's on-disk format (its `publish` is private to `mur-core`).

use std::path::{Path, PathBuf};

use chrono::Utc;
use mur_common::startup_override::claim::{consumed_path, live_path, overrides_dir};
use mur_common::startup_override::{ConfirmedBy, IssuedBy, OVERRIDE_SCHEMA_VERSION, OverrideFile};

use super::super::jobs::{JobState, OverrideJobs};
use super::{QuitClaim, run_claims};

const NAME: &str = "hub-quit-test";
const NONCE: &str = "ovr-0123456789abcdef0123456789abcdef";
const WINDOW: &str = "dashboard";

fn home() -> tempfile::TempDir {
    let t = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(t.path().join("agents").join(NAME)).unwrap();
    std::fs::create_dir_all(overrides_dir(t.path())).unwrap();
    t
}

fn file_bytes() -> Vec<u8> {
    serde_json::to_vec(&OverrideFile {
        version: OVERRIDE_SCHEMA_VERSION,
        nonce: NONCE.into(),
        created_at: Utc::now(),
        issued_by: IssuedBy::Hub,
        confirmed_by: ConfirmedBy::HubNativeDialog,
        no_hitl: true,
        net_unrestricted: false,
        full_perms: false,
    })
    .unwrap()
}

fn audit(home: &Path) -> String {
    std::fs::read_to_string(
        home.join("agents")
            .join(NAME)
            .join("permissions/audit.jsonl"),
    )
    .unwrap_or_default()
}

/// A job that has published, claimed by quit.
fn claimed(jobs: &OverrideJobs) -> Vec<QuitClaim> {
    let entry = jobs.admit(NAME, WINDOW, JobState::Preparing).unwrap();
    jobs.set_state(
        NAME,
        JobState::Published {
            nonce: NONCE.into(),
        },
    );
    let claims = jobs.begin_quit();
    // The job thread's entry drops; the claim keeps the slot.
    drop(entry);
    claims
}

fn live(home: &Path) -> PathBuf {
    live_path(&overrides_dir(home), NAME)
}

#[test]
fn run_claims_withdraws_each_and_removes_the_entry() {
    let h = home();
    std::fs::write(live(h.path()), file_bytes()).unwrap();
    let jobs = OverrideJobs::default();
    let claims = claimed(&jobs);
    assert_eq!(jobs.active_names(), vec![NAME.to_string()]);
    run_claims(h.path(), &jobs, claims);
    assert!(!live(h.path()).exists());
    let a = audit(h.path());
    assert_eq!(a.matches("startup_override_withdrawn").count(), 1, "{a}");
    assert!(a.contains(r#""window":"dashboard""#), "{a}");
    assert!(jobs.active_names().is_empty());
}

#[test]
fn run_claims_with_a_consumed_file_classifies_not_undoes() {
    let h = home();
    std::fs::write(consumed_path(&overrides_dir(h.path()), NAME), file_bytes()).unwrap();
    let jobs = OverrideJobs::default();
    let claims = claimed(&jobs);
    run_claims(h.path(), &jobs, claims);
    let a = audit(h.path());
    assert!(!a.contains("startup_override_withdrawn"), "{a}");
    assert!(a.contains("startup_override_unacknowledged"), "{a}");
    assert!(jobs.active_names().is_empty());
}

mod quit_action_tests {
    use super::super::super::jobs::{JobState, OverrideJobs};
    use super::super::{QuitAction, quit_action};

    #[test]
    fn no_state_or_no_jobs_exits_now() {
        assert_eq!(quit_action(None), QuitAction::ExitNow);
        assert_eq!(
            quit_action(Some(&OverrideJobs::default())),
            QuitAction::ExitNow
        );
    }

    #[test]
    fn active_job_prompts() {
        let jobs = OverrideJobs::default();
        let _entry = jobs.admit("a", "dashboard", JobState::Preparing).unwrap();
        assert_eq!(quit_action(Some(&jobs)), QuitAction::Prompt);
    }

    #[test]
    fn prompt_is_asked_once_while_open() {
        let jobs = OverrideJobs::default();
        assert!(jobs.begin_quit_prompt());
        assert!(!jobs.begin_quit_prompt());
        jobs.end_quit_prompt();
        assert!(jobs.begin_quit_prompt());
    }

    #[test]
    fn already_quitting_exits_now() {
        let jobs = OverrideJobs::default();
        let _entry = jobs.admit("a", "dashboard", JobState::Preparing).unwrap();
        let _ = jobs.begin_quit();
        assert_eq!(quit_action(Some(&jobs)), QuitAction::ExitNow);
    }
}
