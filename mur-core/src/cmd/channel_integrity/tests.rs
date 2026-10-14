use std::path::Path;
use std::sync::Arc;

use mur_channel::ChannelService;
use mur_channel::seal::HeadSealer;
use mur_common::channel::{ChannelActor, EventKind};
use mur_common::identity::{AgentIdentity, RotationAttestation, RotationReason};
use tempfile::TempDir;

use super::*;
use crate::channel_writer::{ROUTER_AGENT, plant_writer_identity};

fn router_dir(home: &Path) -> std::path::PathBuf {
    home.join("agents").join(ROUTER_AGENT)
}

fn events_file(home: &Path, ch: &str) -> std::path::PathBuf {
    home.join("channels").join(ch).join("events.jsonl")
}

/// A channel with `n` real, router-signed, sealed events (the production path).
fn sealed_channel(home: &Path, id: &AgentIdentity, kv: u32, n: usize) -> String {
    let svc = ChannelService::open(home).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    append(home, &ch.id, id, kv, n);
    ch.id
}

fn append(home: &Path, ch: &str, id: &AgentIdentity, kv: u32, n: usize) {
    let svc = ChannelService::open(home).unwrap();
    let sealer = HeadSealer::new(home, Arc::new(id.clone()), kv);
    let hook = move |i: &mur_channel::store::SealInput<'_>| sealer.seal(i);
    for k in 0..n {
        svc.append_signed_with(
            ch,
            id,
            kv,
            ChannelActor::System,
            EventKind::Message,
            serde_json::json!({ "text": format!("m{k}") }),
            None,
            Some(&hook),
        )
        .unwrap();
    }
}

fn verdict(home: &Path, ch: &str) -> Outcome {
    verify_all(home, &[ch.to_string()]).pop().unwrap()
}

/// bootstrap(id0) then one rotation id0 -> id1, written as `rotations.jsonl`.
fn write_rotation(home: &Path, id0: &AgentIdentity, id1: &AgentIdentity) {
    let now = "2026-10-14T00:00:00Z";
    let boot = RotationAttestation::new(
        "u",
        "",
        id0.pubkey_text(),
        0,
        0,
        now,
        RotationReason::Scheduled,
    )
    .into_bootstrap();
    let mut rot = RotationAttestation::new(
        "u",
        id0.pubkey_text(),
        id1.pubkey_text(),
        0,
        1,
        now,
        RotationReason::Scheduled,
    );
    rot.sign(id0.signing_key());
    let text = format!(
        "{}\n{}\n",
        serde_json::to_string(&boot).unwrap(),
        serde_json::to_string(&rot).unwrap()
    );
    std::fs::write(router_dir(home).join("rotations.jsonl"), text).unwrap();
}

#[test]
fn a_sealed_untouched_channel_is_ok() {
    let tmp = TempDir::new().unwrap();
    let id = plant_writer_identity(tmp.path());
    let ch = sealed_channel(tmp.path(), &id, 0, 3);
    let o = verdict(tmp.path(), &ch);
    assert_eq!(o.verdict, Verdict::Ok, "{:?}", o.report.map(|r| r.state));
    assert_eq!(exit_code(&[o]), EXIT_OK);
}

#[test]
fn a_channel_that_was_never_sealed_is_unverified_not_ok() {
    let tmp = TempDir::new().unwrap();
    plant_writer_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    svc.append(
        &ch.id,
        ChannelActor::System,
        EventKind::Message,
        serde_json::json!({"text":"x"}),
        None,
    )
    .unwrap();
    let o = verdict(tmp.path(), &ch.id);
    assert_eq!(o.verdict, Verdict::Unverified);
    assert_eq!(exit_code(&[o]), EXIT_UNVERIFIED);
}

#[test]
fn an_edited_line_is_failed() {
    let tmp = TempDir::new().unwrap();
    let id = plant_writer_identity(tmp.path());
    let ch = sealed_channel(tmp.path(), &id, 0, 3);
    let p = events_file(tmp.path(), &ch);
    let text = std::fs::read_to_string(&p).unwrap();
    assert!(text.contains("m1"));
    std::fs::write(&p, text.replacen("m1", "mX", 1)).unwrap();
    let o = verdict(tmp.path(), &ch);
    assert_eq!(o.verdict, Verdict::Failed);
    assert_eq!(exit_code(&[o]), EXIT_FAILED);
}

#[test]
fn a_deleted_middle_line_is_failed() {
    let tmp = TempDir::new().unwrap();
    let id = plant_writer_identity(tmp.path());
    let ch = sealed_channel(tmp.path(), &id, 0, 4);
    let p = events_file(tmp.path(), &ch);
    let kept: Vec<_> = std::fs::read_to_string(&p)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    std::fs::write(&p, format!("{}\n{}\n{}\n", kept[0], kept[1], kept[3])).unwrap();
    assert_eq!(verdict(tmp.path(), &ch).verdict, Verdict::Failed);
}

#[test]
fn a_forged_line_appended_past_the_head_is_not_ok() {
    let tmp = TempDir::new().unwrap();
    let id = plant_writer_identity(tmp.path());
    let ch = sealed_channel(tmp.path(), &id, 0, 2);
    // A hub-style unsigned append: valid JSON, next seq, past the head.
    let svc = ChannelService::open(tmp.path()).unwrap();
    svc.append(
        &ch,
        ChannelActor::System,
        EventKind::Message,
        serde_json::json!({"text":"forged"}),
        None,
    )
    .unwrap();
    let o = verdict(tmp.path(), &ch);
    assert_eq!(
        o.verdict,
        Verdict::Unverified,
        "tail lines are not tamper-evident"
    );
}

#[test]
fn a_head_signed_with_an_unknown_key_version_is_unverified_not_failed() {
    let tmp = TempDir::new().unwrap();
    let id = plant_writer_identity(tmp.path());
    // Sealed as version 5; the router has no rotation record naming a 5.
    let ch = sealed_channel(tmp.path(), &id, 5, 2);
    let o = verdict(tmp.path(), &ch);
    assert_eq!(o.verdict, Verdict::Unverified);
    assert!(o.unresolved_keys.contains(&Some(5)));
}

#[test]
fn the_current_key_is_not_substituted_for_an_unknown_version() {
    // The library resolver would hand back identity.pub here. Ours must not:
    // that would let any head claiming any version verify against today's key.
    let tmp = TempDir::new().unwrap();
    let id = plant_writer_identity(tmp.path());
    let keys = keys::RouterKeys::load(&router_dir(tmp.path()));
    assert_eq!(keys.resolve(Some(0)), Some(id.verifying_key_bytes()));
    assert_eq!(keys.resolve(Some(7)), None);
    assert_eq!(keys.misses(), vec![Some(7)]);
}

#[test]
fn a_missing_router_key_is_unverified() {
    let tmp = TempDir::new().unwrap();
    let id = plant_writer_identity(tmp.path());
    let ch = sealed_channel(tmp.path(), &id, 0, 2);
    std::fs::remove_dir_all(router_dir(tmp.path())).unwrap();
    assert_eq!(verdict(tmp.path(), &ch).verdict, Verdict::Unverified);
}

#[test]
fn a_wrong_key_is_failed_not_unverified() {
    // Key present and resolvable, signature does not match: that is evidence.
    let tmp = TempDir::new().unwrap();
    plant_writer_identity(tmp.path());
    let other = AgentIdentity::generate();
    let ch = sealed_channel(tmp.path(), &other, 0, 2);
    assert_eq!(verdict(tmp.path(), &ch).verdict, Verdict::Failed);
}

#[test]
fn a_rotated_router_verifies_old_and_new_versions() {
    let tmp = TempDir::new().unwrap();
    let id0 = plant_writer_identity(tmp.path());
    let id1 = AgentIdentity::generate();
    write_rotation(tmp.path(), &id0, &id1);
    // One channel sealed before the rotation, one after.
    let old = sealed_channel(tmp.path(), &id0, 0, 2);
    let new = sealed_channel(tmp.path(), &id1, 1, 2);
    assert_eq!(verdict(tmp.path(), &old).verdict, Verdict::Ok);
    assert_eq!(verdict(tmp.path(), &new).verdict, Verdict::Ok);
}

#[test]
fn a_version_absent_from_a_valid_chain_is_unverified() {
    let tmp = TempDir::new().unwrap();
    let id0 = plant_writer_identity(tmp.path());
    let id1 = AgentIdentity::generate();
    write_rotation(tmp.path(), &id0, &id1);
    let ch = sealed_channel(tmp.path(), &id1, 4, 2);
    assert_eq!(verdict(tmp.path(), &ch).verdict, Verdict::Unverified);
}

#[test]
fn a_rotation_record_with_an_unreadable_line_trusts_nothing() {
    let tmp = TempDir::new().unwrap();
    let id0 = plant_writer_identity(tmp.path());
    let ch = sealed_channel(tmp.path(), &id0, 0, 2);
    assert_eq!(
        verdict(tmp.path(), &ch).verdict,
        Verdict::Ok,
        "precondition"
    );
    std::fs::write(
        router_dir(tmp.path()).join("rotations.jsonl"),
        "{not json\n",
    )
    .unwrap();
    let o = verdict(tmp.path(), &ch);
    assert_eq!(o.verdict, Verdict::Unverified);
}

#[test]
fn a_rotation_record_that_fails_chain_verification_trusts_nothing() {
    let tmp = TempDir::new().unwrap();
    let id0 = plant_writer_identity(tmp.path());
    let ch = sealed_channel(tmp.path(), &id0, 0, 2);
    // Chain whose rotation is signed by the wrong key.
    let id1 = AgentIdentity::generate();
    let mallory = AgentIdentity::generate();
    let now = "2026-10-14T00:00:00Z";
    let boot = RotationAttestation::new(
        "u",
        "",
        id0.pubkey_text(),
        0,
        0,
        now,
        RotationReason::Scheduled,
    )
    .into_bootstrap();
    let mut rot = RotationAttestation::new(
        "u",
        id0.pubkey_text(),
        id1.pubkey_text(),
        0,
        1,
        now,
        RotationReason::Scheduled,
    );
    rot.sign(mallory.signing_key());
    std::fs::write(
        router_dir(tmp.path()).join("rotations.jsonl"),
        format!(
            "{}\n{}\n",
            serde_json::to_string(&boot).unwrap(),
            serde_json::to_string(&rot).unwrap()
        ),
    )
    .unwrap();
    assert_eq!(verdict(tmp.path(), &ch).verdict, Verdict::Unverified);
}

#[test]
fn an_unreadable_line_after_the_head_keeps_an_intact_channel_from_ok() {
    let tmp = TempDir::new().unwrap();
    let id = plant_writer_identity(tmp.path());
    let ch = sealed_channel(tmp.path(), &id, 0, 2);
    let p = events_file(tmp.path(), &ch);
    let mut text = std::fs::read_to_string(&p).unwrap();
    text.push_str("garbage that is no event\n");
    std::fs::write(&p, text).unwrap();
    let o = verdict(tmp.path(), &ch);
    assert_eq!(o.verdict, Verdict::Unverified);
    assert_eq!(o.report.unwrap().unparseable_lines.len(), 1);
}

#[test]
fn a_head_with_no_log_is_unverified() {
    let tmp = TempDir::new().unwrap();
    let id = plant_writer_identity(tmp.path());
    let ch = sealed_channel(tmp.path(), &id, 0, 1);
    std::fs::remove_file(events_file(tmp.path(), &ch)).unwrap();
    assert_eq!(verdict(tmp.path(), &ch).verdict, Verdict::Unverified);
}

#[test]
fn an_unreadable_channel_is_unverified_with_the_error() {
    let tmp = TempDir::new().unwrap();
    plant_writer_identity(tmp.path());
    // An id that cannot name a head file makes the library refuse.
    let o = verdict(tmp.path(), "a/b");
    assert_eq!(o.verdict, Verdict::Unverified);
    assert!(o.report.is_none());
    assert!(o.error.is_some());
}

#[test]
fn a_failed_channel_outranks_an_unverified_one_in_the_exit_code() {
    let tmp = TempDir::new().unwrap();
    let id = plant_writer_identity(tmp.path());
    let good = sealed_channel(tmp.path(), &id, 0, 2);
    let bad = sealed_channel(tmp.path(), &id, 0, 2);
    let p = events_file(tmp.path(), &bad);
    std::fs::write(
        &p,
        std::fs::read_to_string(&p).unwrap().replacen("m0", "mX", 1),
    )
    .unwrap();
    let unsealed = {
        let svc = ChannelService::open(tmp.path()).unwrap();
        let c = svc.create_for_workflow("g").unwrap();
        svc.append(
            &c.id,
            ChannelActor::System,
            EventKind::Message,
            serde_json::json!({"text":"x"}),
            None,
        )
        .unwrap();
        c.id
    };
    let all = verify_all(tmp.path(), &[good.clone(), bad, unsealed.clone()]);
    assert_eq!(exit_code(&all), EXIT_FAILED);
    let no_bad = verify_all(tmp.path(), &[good, unsealed]);
    assert_eq!(exit_code(&no_bad), EXIT_UNVERIFIED);
    assert_eq!(exit_code(&[]), EXIT_OK);
}

#[test]
fn a_key_miss_in_one_channel_does_not_taint_the_next() {
    let tmp = TempDir::new().unwrap();
    let id = plant_writer_identity(tmp.path());
    let wrong_version = sealed_channel(tmp.path(), &id, 5, 1);
    let fine = sealed_channel(tmp.path(), &id, 0, 1);
    let all = verify_all(tmp.path(), &[wrong_version, fine]);
    assert_eq!(all[0].verdict, Verdict::Unverified);
    assert_eq!(all[1].verdict, Verdict::Ok);
}

#[test]
fn run_with_no_ids_checks_every_channel_and_returns_the_code() {
    let tmp = TempDir::new().unwrap();
    let id = plant_writer_identity(tmp.path());
    sealed_channel(tmp.path(), &id, 0, 1);
    sealed_channel(tmp.path(), &id, 0, 1);
    let mut out = Vec::new();
    let code = run(tmp.path(), &[], false, &mut out).unwrap();
    assert_eq!(code, EXIT_OK);
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("2 channel(s): 2 ok, 0 unverified, 0 failed")
    );
}

#[test]
fn run_on_an_empty_home_says_so_and_exits_zero() {
    let tmp = TempDir::new().unwrap();
    let mut out = Vec::new();
    assert_eq!(run(tmp.path(), &[], false, &mut out).unwrap(), EXIT_OK);
    assert!(
        String::from_utf8(out)
            .unwrap()
            .contains("No channels to verify")
    );
}

#[test]
fn human_output_names_the_verdict_and_the_reason() {
    let tmp = TempDir::new().unwrap();
    let id = plant_writer_identity(tmp.path());
    let ch = sealed_channel(tmp.path(), &id, 5, 1);
    let mut out = Vec::new();
    run(tmp.path(), std::slice::from_ref(&ch), false, &mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains(&ch));
    assert!(text.contains("UNVERIFIED"), "{text}");
    assert!(
        text.contains("not in the router's rotation record"),
        "{text}"
    );
    assert!(text.contains("not genuine"), "{text}");
}

#[test]
fn json_output_is_a_stable_document() {
    let tmp = TempDir::new().unwrap();
    let id = plant_writer_identity(tmp.path());
    let ok = sealed_channel(tmp.path(), &id, 0, 2);
    let bad = sealed_channel(tmp.path(), &id, 0, 3);
    let p = events_file(tmp.path(), &bad);
    std::fs::write(
        &p,
        std::fs::read_to_string(&p).unwrap().replacen("m1", "mX", 1),
    )
    .unwrap();
    let mut out = Vec::new();
    let code = run(tmp.path(), &[ok.clone(), bad.clone()], true, &mut out).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(code, EXIT_FAILED);
    assert_eq!(v["verdict"], "failed");
    assert_eq!(v["exit_code"], 1);
    let ch = v["channels"].as_array().unwrap();
    assert_eq!(ch[0]["channel_id"], ok.as_str());
    assert_eq!(ch[0]["verdict"], "ok");
    assert_eq!(ch[0]["state"], "intact");
    assert_eq!(ch[0]["lines"]["sealed"], 2);
    assert_eq!(ch[1]["verdict"], "failed");
    assert_eq!(ch[1]["state"], "chain_broken");
}

#[test]
fn json_carries_the_error_for_an_unreadable_channel() {
    let tmp = TempDir::new().unwrap();
    let mut out = Vec::new();
    run(tmp.path(), &["a/b".to_string()], true, &mut out).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["channels"][0]["verdict"], "unverified");
    assert!(v["channels"][0]["state"].is_null());
    assert!(v["channels"][0]["error"].is_string());
}

#[test]
fn verification_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let id = plant_writer_identity(tmp.path());
    let ch = sealed_channel(tmp.path(), &id, 0, 3);
    let snap = |home: &Path| {
        let mut v = Vec::new();
        for e in walk(home) {
            v.push((e.clone(), std::fs::read(&e).unwrap()));
        }
        v.sort();
        v
    };
    let before = snap(tmp.path());
    let mut out = Vec::new();
    run(tmp.path(), &[ch], false, &mut out).unwrap();
    assert_eq!(before, snap(tmp.path()));
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// Router bootstrap + one rotation id0 -> id1 of the given reason.
fn rotation_text(id0: &AgentIdentity, id1: &AgentIdentity, reason: RotationReason) -> String {
    let now = "2026-10-14T00:00:00Z";
    let boot = RotationAttestation::new(
        "u",
        "",
        id0.pubkey_text(),
        0,
        0,
        now,
        RotationReason::Scheduled,
    )
    .into_bootstrap();
    let emergency = reason == RotationReason::Emergency;
    let mut rot =
        RotationAttestation::new("u", id0.pubkey_text(), id1.pubkey_text(), 0, 1, now, reason);
    if !emergency {
        rot.sign(id0.signing_key());
    }
    format!(
        "{}\n{}\n",
        serde_json::to_string(&boot).unwrap(),
        serde_json::to_string(&rot).unwrap()
    )
}

#[test]
fn one_unreadable_line_inside_an_otherwise_valid_rotation_record_trusts_nothing() {
    // A parser that skips the bad line would still see a valid chain.
    let tmp = TempDir::new().unwrap();
    let id0 = plant_writer_identity(tmp.path());
    let id1 = AgentIdentity::generate();
    let ch = sealed_channel(tmp.path(), &id1, 1, 2);
    let path = router_dir(tmp.path()).join("rotations.jsonl");
    std::fs::write(&path, rotation_text(&id0, &id1, RotationReason::Scheduled)).unwrap();
    assert_eq!(
        verdict(tmp.path(), &ch).verdict,
        Verdict::Ok,
        "precondition"
    );
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("{this line is damaged\n");
    std::fs::write(&path, text).unwrap();
    assert_eq!(verdict(tmp.path(), &ch).verdict, Verdict::Unverified);
}

#[test]
fn an_emergency_rotation_is_not_trusted() {
    // Emergency rotations carry no signature from the old key, so they prove
    // no continuity. A head sealed under the post-emergency key is unverified.
    let tmp = TempDir::new().unwrap();
    let id0 = plant_writer_identity(tmp.path());
    let id1 = AgentIdentity::generate();
    let ch = sealed_channel(tmp.path(), &id1, 1, 2);
    std::fs::write(
        router_dir(tmp.path()).join("rotations.jsonl"),
        rotation_text(&id0, &id1, RotationReason::Emergency),
    )
    .unwrap();
    assert_eq!(verdict(tmp.path(), &ch).verdict, Verdict::Unverified);
}

#[test]
fn a_line_signed_under_an_unresolvable_version_keeps_an_intact_chain_from_ok() {
    // The head verifies and the chain is intact, but one sealed line names a key
    // version the router's record does not hold: part of the log is unchecked.
    let tmp = TempDir::new().unwrap();
    let id = plant_writer_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap().id;
    // Seal once so a head exists (otherwise the first line is `legacy`, which is
    // never key-checked), then a line signed under version 9 that no seal wrote,
    // then seal again so that line is inside the chain.
    append(tmp.path(), &ch, &id, 0, 1);
    svc.append_signed_with(
        &ch,
        &id,
        9,
        ChannelActor::System,
        EventKind::Message,
        serde_json::json!({"text":"old"}),
        None,
        None,
    )
    .unwrap();
    append(tmp.path(), &ch, &id, 0, 1);
    let o = verdict(tmp.path(), &ch);
    assert_eq!(
        o.report.as_ref().unwrap().state,
        mur_channel::verify::IntegrityState::Intact
    );
    assert_eq!(o.unresolved_keys, vec![Some(9)]);
    assert_eq!(o.verdict, Verdict::Unverified);
}

#[test]
fn a_seq_gap_in_an_unsealed_channel_is_failed_not_merely_unverified() {
    let tmp = TempDir::new().unwrap();
    plant_writer_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap().id;
    for k in 0..4 {
        svc.append(
            &ch,
            ChannelActor::System,
            EventKind::Message,
            serde_json::json!({ "text": format!("m{k}") }),
            None,
        )
        .unwrap();
    }
    let p = events_file(tmp.path(), &ch);
    let lines: Vec<String> = std::fs::read_to_string(&p)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    let kept: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 2)
        .map(|(_, l)| l.as_str())
        .collect();
    std::fs::write(&p, format!("{}\n", kept.join("\n"))).unwrap();
    let o = verdict(tmp.path(), &ch);
    assert_eq!(
        o.report.as_ref().unwrap().state,
        mur_channel::verify::IntegrityState::NoHead
    );
    assert_eq!(
        o.verdict,
        Verdict::Failed,
        "a hole is evidence even with no head"
    );
}

#[test]
fn output_is_flushed_before_the_caller_can_exit() {
    struct Spy {
        flushed: bool,
    }
    impl Write for Spy {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flushed = true;
            Ok(())
        }
    }
    let tmp = TempDir::new().unwrap();
    let mut spy = Spy { flushed: false };
    run(tmp.path(), &[], false, &mut spy).unwrap();
    assert!(spy.flushed, "process::exit does not flush; run() must");
}
