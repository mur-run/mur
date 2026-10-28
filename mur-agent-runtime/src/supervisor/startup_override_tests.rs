use super::startup_override::*;
use mur_common::agent::OverrideFlags;
use mur_common::startup_override::claim::{consumed_path, live_path, overrides_dir};
use mur_common::startup_override::{ConfirmedBy, REASON_UNSUPPORTED_VERSION};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const AGENT: &str = "alpha";
const STOP: u64 = 15;
const NONCE: &str = "ovr-0123456789abcdef0123456789abcdef";

struct Homes {
    _tmp: tempfile::TempDir,
    mur_home: PathBuf,
    agent_home: PathBuf,
}

fn homes() -> Homes {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join("mur");
    let agent_home = mur_home.join("agents").join(AGENT);
    std::fs::create_dir_all(&agent_home).unwrap();
    std::fs::create_dir_all(overrides_dir(&mur_home)).unwrap();
    Homes {
        _tmp: tmp,
        mur_home,
        agent_home,
    }
}

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

fn publish(h: &Homes, body: &Value) {
    let path = live_path(&overrides_dir(&h.mur_home), AGENT);
    std::fs::write(path, serde_json::to_vec(body).unwrap()).unwrap();
}

fn valid_file() -> Value {
    json!({
        "version": 1,
        "nonce": NONCE,
        "created_at": now().to_rfc3339(),
        "issued_by": "cli",
        "confirmed_by": "tty",
        "no_hitl": false,
        "net_unrestricted": false,
        "full_perms": false,
    })
}

fn consume(h: &Homes) -> Pending {
    consume_at_boot(&h.mur_home, &h.agent_home, AGENT, STOP, now())
}

fn audit_lines(agent_home: &Path) -> Vec<Value> {
    let path = agent_home.join("permissions").join("audit.jsonl");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn live_exists(h: &Homes) -> bool {
    live_path(&overrides_dir(&h.mur_home), AGENT).exists()
}

#[test]
fn absent_file_boots_normally() {
    let h = homes();
    let p = consume(&h);
    assert!(matches!(p, Pending::None));
    assert_eq!(
        finish_after_seal(p, true, vec![], &h.agent_home),
        (None, None)
    );
    assert!(audit_lines(&h.agent_home).is_empty());
}

#[test]
fn missing_overrides_dir_boots_normally() {
    let h = homes();
    std::fs::remove_dir(overrides_dir(&h.mur_home)).unwrap();
    assert!(matches!(consume(&h), Pending::None));
}

#[test]
fn valid_all_false_file_is_consumed_and_recorded() {
    let h = homes();
    publish(&h, &valid_file());
    let p = consume(&h);
    assert!(!live_exists(&h));
    assert!(consumed_path(&overrides_dir(&h.mur_home), AGENT).exists());
    let (ovr, rej) = finish_after_seal(p, true, vec![], &h.agent_home);
    let ovr = ovr.expect("accepted override is recorded");
    assert_eq!(ovr.nonce, NONCE);
    assert_eq!(ovr.flags, OverrideFlags::default());
    assert_eq!(ovr.confirmed_by, ConfirmedBy::Tty);
    assert_eq!(rej, None);
    let lines = audit_lines(&h.agent_home);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["type"], "startup_override_applied");
    assert_eq!(lines[0]["nonce"], NONCE);
    assert_eq!(lines[0]["confirmed_by"], "tty");
}

#[test]
fn second_boot_after_consume_is_normal_and_logs_one_shot() {
    let h = homes();
    publish(&h, &valid_file());
    let first = consume(&h);
    finish_after_seal(first, true, vec![], &h.agent_home);

    let second = consume(&h);
    assert!(matches!(second, Pending::None));
    assert_eq!(
        finish_after_seal(second, true, vec![], &h.agent_home),
        (None, None)
    );
    let lines = audit_lines(&h.agent_home);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["type"], "startup_override_not_reapplied");
}

#[test]
fn rejected_file_is_still_consumed() {
    let h = homes();
    let mut body = valid_file();
    body["version"] = json!(9);
    publish(&h, &body);
    let p = consume(&h);
    assert!(
        !live_exists(&h),
        "a rejected file is never left for the next boot"
    );
    let (ovr, rej) = finish_after_seal(p, true, vec![], &h.agent_home);
    assert_eq!(ovr, None);
    let rej = rej.expect("rejection is recorded");
    assert_eq!(rej.reason, REASON_UNSUPPORTED_VERSION);
    assert_eq!(rej.nonce.as_deref(), Some(NONCE));
}

#[test]
fn unreadable_consumed_file_is_rejected_without_a_nonce() {
    let h = homes();
    std::fs::write(
        live_path(&overrides_dir(&h.mur_home), AGENT),
        b"\xff\xfe not json",
    )
    .unwrap();
    let (ovr, rej) = finish_after_seal(consume(&h), true, vec![], &h.agent_home);
    assert_eq!(ovr, None);
    assert_eq!(rej.unwrap().nonce, None);
}

#[cfg(unix)]
#[test]
fn rename_failure_sets_override_rejected_with_no_nonce() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("SKIP: running as root");
        return;
    }
    let h = homes();
    publish(&h, &valid_file());
    let dir = overrides_dir(&h.mur_home);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let p = consume(&h);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let (ovr, rej) = finish_after_seal(p, true, vec![], &h.agent_home);
    assert_eq!(ovr, None);
    let rej = rej.expect("claim failure is recorded");
    assert_eq!(rej.nonce, None);
    assert!(
        rej.reason.starts_with(REASON_CLAIM_FAILED_PREFIX),
        "{}",
        rej.reason
    );
}

#[test]
fn accepted_on_unsealed_boot_is_rejected_unsealed() {
    let h = homes();
    publish(&h, &valid_file());
    let (ovr, rej) = finish_after_seal(consume(&h), false, vec![], &h.agent_home);
    assert_eq!(ovr, None);
    let rej = rej.expect("unsealed boot reports the override as rejected");
    assert_eq!(rej.reason, REASON_UNSEALED);
    assert_eq!(rej.nonce.as_deref(), Some(NONCE));
    let lines = audit_lines(&h.agent_home);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["type"], "startup_override_rejected");
    assert_eq!(lines[0]["reason"], REASON_UNSEALED);
}

#[test]
fn profile_yaml_is_byte_identical_after_consume() {
    let h = homes();
    let profile = h.agent_home.join("profile.yaml");
    let bytes = b"name: alpha\n# keep me exactly\n".to_vec();
    std::fs::write(&profile, &bytes).unwrap();
    publish(&h, &valid_file());
    finish_after_seal(consume(&h), true, vec![], &h.agent_home);
    assert_eq!(std::fs::read(&profile).unwrap(), bytes);
}

#[test]
fn audit_jsonl_has_one_line_per_outcome() {
    let h = homes();
    // applied
    publish(&h, &valid_file());
    finish_after_seal(consume(&h), true, vec![], &h.agent_home);
    // rejected (validation)
    let mut bad = valid_file();
    bad["version"] = json!(9);
    publish(&h, &bad);
    finish_after_seal(consume(&h), true, vec![], &h.agent_home);
    // rejected (unsealed)
    publish(&h, &valid_file());
    finish_after_seal(consume(&h), false, vec![], &h.agent_home);

    let kinds: Vec<String> = audit_lines(&h.agent_home)
        .iter()
        .map(|l| l["type"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        kinds,
        [
            "startup_override_applied",
            "startup_override_rejected",
            "startup_override_rejected",
        ]
    );
}

/// `pin::refuse` exits between `consume_at_boot` and `finish_after_seal`, so
/// a pre-seal rejection must already be in the audit log when consume returns.
#[test]
fn pre_seal_rejection_is_audited_even_if_boot_is_refused_before_seal() {
    let h = homes();
    let mut body = valid_file();
    body["version"] = json!(9);
    publish(&h, &body);
    let p = consume(&h);
    assert!(matches!(p, Pending::Rejected(_)));
    // No finish_after_seal: the boot was refused (pin::refuse exits).
    let lines = audit_lines(&h.agent_home);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["type"], "startup_override_rejected");
    assert_eq!(lines[0]["nonce"], NONCE);
    assert_eq!(lines[0]["reason"], REASON_UNSUPPORTED_VERSION);
}

#[test]
fn pre_seal_rejection_is_audited_exactly_once_on_a_full_boot() {
    let h = homes();
    let mut bad = valid_file();
    bad["version"] = json!(9);
    publish(&h, &bad);
    let (_, rej) = finish_after_seal(consume(&h), true, vec![], &h.agent_home);
    assert!(rej.is_some(), "the lock still records the rejection");
    let lines = audit_lines(&h.agent_home);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["type"], "startup_override_rejected");
}

/// A duplicate boot that loses `running.lock` must not touch the override:
/// the file stays live for the boot that holds the lock, and the loser
/// writes no audit line.
#[test]
fn a_boot_that_loses_the_lock_leaves_the_override_for_the_winner() {
    let h = homes();
    publish(&h, &valid_file());
    let lock_path = h.agent_home.join("running.lock");
    let _held = crate::lock_file::LockHandle::acquire(&lock_path).unwrap();

    let lost = lock_then_consume(&lock_path, &h.mur_home, &h.agent_home, AGENT, STOP, now());

    assert!(lost.is_err(), "second boot must lose the lock");
    assert!(live_exists(&h), "loser consumed the override");
    assert!(audit_lines(&h.agent_home).is_empty(), "loser wrote audit");

    drop(_held);
    let (_lock, pending) =
        lock_then_consume(&lock_path, &h.mur_home, &h.agent_home, AGENT, STOP, now()).unwrap();
    assert!(matches!(pending, Pending::Accepted(_)), "{pending:?}");
}

/// The lock is now taken before `pin::verify`, so a refused boot exits holding
/// it without ever writing `running.lock`. Dropping the handle (what process
/// exit does) must leave the agent startable and not look running.
#[test]
fn a_boot_refused_after_the_lock_leaves_the_agent_startable() {
    let h = homes();
    let lock_path = h.agent_home.join("running.lock");
    let (refused, _) =
        lock_then_consume(&lock_path, &h.mur_home, &h.agent_home, AGENT, STOP, now()).unwrap();
    drop(refused); // no `release()`: `pin::refuse` exits the process instead

    assert!(crate::lock_file::is_stale(&lock_path).unwrap());
    assert!(crate::lock_file::LockHandle::acquire(&lock_path).is_ok());
}

#[test]
fn accepted_no_hitl_builds_a_waiver() {
    let h = homes();
    let mut body = valid_file();
    body["no_hitl"] = json!(true);
    publish(&h, &body);
    let p = consume(&h);
    let w = waiver(&p, true).expect("an accepted no_hitl on a sealed boot");
    assert_eq!(w.nonce(), NONCE);
}

#[test]
fn rejected_absent_or_flagless_builds_none() {
    let h = homes();
    assert!(waiver(&consume(&h), true).is_none(), "absent");

    let h = homes();
    publish(&h, &valid_file());
    assert!(waiver(&consume(&h), true).is_none(), "accepted, no flag");

    let h = homes();
    let mut body = valid_file();
    body["no_hitl"] = json!(true);
    body["version"] = json!(9);
    publish(&h, &body);
    assert!(waiver(&consume(&h), true).is_none(), "rejected");
}

/// An unsealed boot reports the override as rejected, so it must not apply it
/// either: the runner gets no waiver.
#[test]
fn unsealed_no_hitl_is_recorded_rejected_and_not_applied() {
    let h = homes();
    let mut body = valid_file();
    body["no_hitl"] = json!(true);
    publish(&h, &body);
    let p = consume(&h);
    assert!(waiver(&p, false).is_none());
    let (ovr, rej) = finish_after_seal(p, false, vec![], &h.agent_home);
    assert_eq!(ovr, None);
    assert_eq!(rej.expect("recorded").reason, REASON_UNSEALED);
}

fn accepted(net: bool, full: bool) -> Pending {
    Pending::Accepted(mur_common::agent::OverrideRecord {
        nonce: NONCE.into(),
        flags: OverrideFlags {
            no_hitl: false,
            net_unrestricted: net,
            full_perms: full,
        },
        confirmed_by: ConfirmedBy::Tty,
    })
}

fn restricted_profile() -> mur_common::AgentProfile {
    let mut p = mur_common::AgentProfile::default_for_tests();
    p.entitlements.network.outbound.mode = mur_common::agent::NetworkOutboundMode::Restricted;
    p
}

#[test]
fn only_an_accepted_override_merges() {
    for pending in [
        Pending::None,
        Pending::Rejected(mur_common::agent::OverrideRejection {
            nonce: None,
            reason: "x".into(),
        }),
    ] {
        let mut p = restricted_profile();
        let before = p.clone();
        assert!(merge_into(&pending, &mut p).is_none());
        assert_eq!(p, before);
    }
}

#[test]
fn a_sealed_boot_keeps_the_relaxation_and_the_file_as_diff_base() {
    let mut p = restricted_profile();
    let before = p.clone();
    let merged = merge_into(&accepted(true, false), &mut p);
    assert_eq!(
        p.entitlements.network.outbound.mode,
        mur_common::agent::NetworkOutboundMode::Unrestricted
    );
    let relaxed = p.clone();
    let (on_disk, changes) = settle(merged, true, &mut p);
    assert_eq!(p, relaxed);
    assert_eq!(on_disk, Some(before));
    assert_eq!(changes.len(), 1);
}

#[test]
fn an_unsealed_boot_withdraws_the_relaxation() {
    let mut p = restricted_profile();
    let before = p.clone();
    let merged = merge_into(&accepted(true, true), &mut p);
    assert_ne!(p, before);
    let (on_disk, changes) = settle(merged, false, &mut p);
    assert_eq!(p, before, "reported rejected means never applied");
    assert_eq!(on_disk, None);
    assert!(changes.is_empty());
}

#[test]
fn the_applied_audit_line_carries_the_change_summary() {
    let h = homes();
    let changes = vec!["network.outbound.mode: restricted -> unrestricted".to_string()];
    finish_after_seal(accepted(true, false), true, changes.clone(), &h.agent_home);
    let lines = audit_lines(&h.agent_home);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["type"], "startup_override_applied");
    assert_eq!(lines[0]["changes"], json!(changes));
}
