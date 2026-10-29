use chrono::Utc;
use mur_common::startup_override::claim::live_path;
use mur_common::startup_override::{ConfirmedBy, IssuedBy, OVERRIDE_SCHEMA_VERSION, OverrideFile};

use super::super::outcome::Cleanup;
use super::super::test_support::*;
use super::super::writer;
use super::*;

const WINDOW: &str = "dashboard";

/// What a Hub job leaves behind once it has published.
fn publish(home: &Home) -> String {
    let nonce = writer::new_nonce().unwrap();
    let file = OverrideFile {
        version: OVERRIDE_SCHEMA_VERSION,
        nonce: nonce.clone(),
        created_at: Utc::now(),
        issued_by: IssuedBy::Hub,
        confirmed_by: ConfirmedBy::HubNativeDialog,
        no_hitl: true,
        net_unrestricted: false,
        full_perms: false,
    };
    writer::publish(&home.dir(), NAME, &file).unwrap();
    nonce
}

#[test]
fn an_unclaimed_file_is_withdrawn_and_audited_with_the_window() {
    let home = Home::new();
    let nonce = publish(&home);
    match withdraw_on_quit(home.path(), NAME, &nonce, WINDOW) {
        OverrideOutcome::Failed {
            step: Step::Cancelled,
            cleanup: Some(Cleanup::Withdrawn),
            ..
        } => {}
        other => panic!("{other:?}"),
    }
    assert!(!live_path(&home.dir(), NAME).exists());
    let audit = home.audit();
    assert!(audit.contains("startup_override_withdrawn"), "{audit}");
    assert!(audit.contains(&nonce), "{audit}");
    assert!(audit.contains(r#""window":"dashboard""#), "{audit}");
}

#[test]
fn a_boot_that_already_applied_it_is_reported_not_undone() {
    let home = Home::new();
    let nonce = publish(&home);
    boot(home.path(), Boot::Applies);
    match withdraw_on_quit(home.path(), NAME, &nonce, WINDOW) {
        OverrideOutcome::Applied { late: true, .. } => {}
        other => panic!("{other:?}"),
    }
    // The runtime audits what it applied; the quit path adds no line.
    assert!(!home.audit().contains("startup_override_withdrawn"));
}

#[test]
fn a_silent_consume_is_c() {
    let home = Home::new();
    let nonce = publish(&home);
    boot(home.path(), Boot::ConsumesSilently);
    match withdraw_on_quit(home.path(), NAME, &nonce, WINDOW) {
        OverrideOutcome::Failed {
            cleanup: Some(Cleanup::ConsumedUnacknowledged),
            ..
        } => {}
        other => panic!("{other:?}"),
    }
    assert!(home.audit().contains("startup_override_unacknowledged"));
}

#[test]
fn the_quit_path_takes_no_writer_lock() {
    let home = Home::new();
    // A job still holds the writer lock while the Hub quits.
    let _held = writer::lock(&home.dir(), NAME).unwrap();
    let nonce = publish(&home);
    assert!(matches!(
        withdraw_on_quit(home.path(), NAME, &nonce, WINDOW),
        OverrideOutcome::Failed {
            cleanup: Some(Cleanup::Withdrawn),
            ..
        }
    ));
}
