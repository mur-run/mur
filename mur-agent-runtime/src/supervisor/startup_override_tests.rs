use super::startup_override::*;
use mur_common::agent::OverrideFlags;
use mur_common::startup_override::claim::{consumed_path, live_path, overrides_dir};
use mur_common::startup_override::{ConfirmedBy, REASON_UNSUPPORTED_FLAG_PREFIX};
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
    assert_eq!(finish_after_seal(p, true, &h.agent_home), (None, None));
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
    let (ovr, rej) = finish_after_seal(p, true, &h.agent_home);
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
    finish_after_seal(first, true, &h.agent_home);

    let second = consume(&h);
    assert!(matches!(second, Pending::None));
    assert_eq!(finish_after_seal(second, true, &h.agent_home), (None, None));
    let lines = audit_lines(&h.agent_home);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["type"], "startup_override_not_reapplied");
}

#[test]
fn rejected_file_is_still_consumed() {
    let h = homes();
    let mut body = valid_file();
    body["no_hitl"] = json!(true);
    publish(&h, &body);
    let p = consume(&h);
    assert!(
        !live_exists(&h),
        "a rejected file is never left for the next boot"
    );
    let (ovr, rej) = finish_after_seal(p, true, &h.agent_home);
    assert_eq!(ovr, None);
    let rej = rej.expect("rejection is recorded");
    assert_eq!(
        rej.reason,
        format!("{REASON_UNSUPPORTED_FLAG_PREFIX}no_hitl")
    );
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
    let (ovr, rej) = finish_after_seal(consume(&h), true, &h.agent_home);
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
    let (ovr, rej) = finish_after_seal(p, true, &h.agent_home);
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
    let (ovr, rej) = finish_after_seal(consume(&h), false, &h.agent_home);
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
    finish_after_seal(consume(&h), true, &h.agent_home);
    assert_eq!(std::fs::read(&profile).unwrap(), bytes);
}

#[test]
fn audit_jsonl_has_one_line_per_outcome() {
    let h = homes();
    // applied
    publish(&h, &valid_file());
    finish_after_seal(consume(&h), true, &h.agent_home);
    // rejected (validation)
    let mut bad = valid_file();
    bad["version"] = json!(9);
    publish(&h, &bad);
    finish_after_seal(consume(&h), true, &h.agent_home);
    // rejected (unsealed)
    publish(&h, &valid_file());
    finish_after_seal(consume(&h), false, &h.agent_home);

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
    body["no_hitl"] = json!(true);
    publish(&h, &body);
    let p = consume(&h);
    assert!(matches!(p, Pending::Rejected(_)));
    // No finish_after_seal: the boot was refused (pin::refuse exits).
    let lines = audit_lines(&h.agent_home);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["type"], "startup_override_rejected");
    assert_eq!(lines[0]["nonce"], NONCE);
    assert_eq!(
        lines[0]["reason"],
        format!("{REASON_UNSUPPORTED_FLAG_PREFIX}no_hitl")
    );
}

#[test]
fn pre_seal_rejection_is_audited_exactly_once_on_a_full_boot() {
    let h = homes();
    let mut bad = valid_file();
    bad["version"] = json!(9);
    publish(&h, &bad);
    let (_, rej) = finish_after_seal(consume(&h), true, &h.agent_home);
    assert!(rej.is_some(), "the lock still records the rejection");
    let lines = audit_lines(&h.agent_home);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["type"], "startup_override_rejected");
}
