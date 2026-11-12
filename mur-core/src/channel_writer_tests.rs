use super::*;
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

/// `REFUSALS` is process-wide, and `cargo test` runs these in parallel in
/// one process — so any test that asserts on a COUNT must hold this lock.
/// Without it, an unrelated test recording a refusal lands inside another
/// test's window and the assertion fails for a reason that has nothing to
/// do with the behaviour under test.
pub(crate) static COUNTER: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// An UNREADABLE writer key must not silently produce an unsigned event.
///
/// This is the write-side mirror of `channel_verify::verify_event`: there,
/// an unreadable key must not pass as an absent signature; here, it must
/// not pass as "no key, so unsigned is fine". A sandboxed `mur` cannot read
/// a sibling's signing key (#975), so before this every channel event such
/// a process wrote was unsigned — and with `require_sig` off the reader
/// accepted it, so the signing guarantee lapsed with no signal anywhere.
#[cfg(unix)]
#[test]
fn an_unreadable_writer_key_does_not_silently_write_unsigned() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let agent_home = home.join("agents").join("mur");
    std::fs::create_dir_all(&agent_home).unwrap();
    AgentIdentity::generate().save(&agent_home).unwrap();
    // The private half lives under `keys/<name>/` for an agent home
    // (#850 option (c)), so chmod the file where it actually is.
    let key = mur_common::identity::private_key_dir(&agent_home).join("identity.key");
    let svc = ChannelService::open(home).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();

    // Precondition: with the key readable, the event IS signed.
    let signed = append_as_writer(
        &svc,
        home,
        &ch.id,
        "mur",
        ChannelActor::System,
        EventKind::Message,
        serde_json::json!({"text": "before"}),
        None,
    )
    .unwrap();
    assert!(signed.sig.is_some(), "precondition: signing works");

    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).unwrap();
    // MUR_CHANNEL_REQUIRE_SIG is not set in the test env, so this takes the
    // warn-and-write path — the event is unsigned, but LOUDLY so. What is
    // asserted here is the discrimination itself: the loader must report
    // this as Denied, not NotFound, which is what makes the warning
    // possible at all.
    let err = mur_common::identity::AgentIdentity::load(&agent_home).unwrap_err();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();

    assert!(
        matches!(err, mur_common::identity::IdentityError::Denied(_)),
        "an unreadable writer key must be distinguishable from an absent \
         one, or the unsigned fallback stays silent: {err:?}"
    );
}

/// A refusal must leave something behind that outlives the process.
///
/// `writer_key` returning `Err` is not enough on its own: `dag.rs` drops
/// that error at three call sites (`let _ = append_as_writer(..)`), and a
/// sandboxed child's stderr is not reliably captured. Without this
/// sidecar, a refused write and a write that never happened look
/// identical after the fact.
#[test]
fn a_refusal_leaves_a_readable_trace() {
    // Bumps the process-global counter twice, so it must take the same
    // lock as the counting tests or it lands inside their window.
    let _guard = COUNTER.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let sidecar = home.join("channels").join("write-refusals.jsonl");
    assert!(!sidecar.exists(), "precondition: no refusals yet");

    record_refusal(home, "mur", "writer key unreadable (test)");

    let body = std::fs::read_to_string(&sidecar).expect("refusal sidecar must exist");
    let rec: serde_json::Value = serde_json::from_str(body.trim()).expect("one JSON line");
    assert_eq!(rec["router_agent"], "mur");
    assert!(
        rec["reason"].as_str().unwrap().contains("unreadable"),
        "the reason must survive, not just the fact of a refusal: {rec}"
    );
    assert!(
        rec["ts"].is_string(),
        "a refusal without a time is not evidence"
    );

    // Append-only: a second refusal must not overwrite the first.
    record_refusal(home, "mur", "second");
    let body = std::fs::read_to_string(&sidecar).unwrap();
    assert_eq!(body.lines().count(), 2, "refusals accumulate, not replace");
}

/// The counter must survive the sidecar failing.
///
/// A closing summary that only counts refusals it managed to WRITE would
/// go quiet in the worst case — an unwritable home, where every refusal
/// is lost and the summary is the last line of defence. Counting happens
/// before the write is attempted, so this holds.
#[test]
fn a_refusal_is_counted_even_when_the_sidecar_cannot_be_written() {
    let _guard = COUNTER.lock().unwrap_or_else(|e| e.into_inner());
    let mark = refusal_count();
    // A path under a regular FILE: create_dir_all cannot succeed here,
    // so record_refusal takes its early return without writing anything.
    let tmp = TempDir::new().unwrap();
    let not_a_dir = tmp.path().join("occupied");
    std::fs::write(&not_a_dir, b"x").unwrap();

    record_refusal(&not_a_dir, "mur", "sidecar unwritable");

    assert_eq!(
        refusals_since(mark),
        1,
        "a refusal that could not be written down is the one most worth \
         counting — otherwise the run reports a clean history it does not have"
    );
}

/// `refusals_since` reports a window, not a total.
#[test]
fn refusals_since_counts_only_what_followed_the_mark() {
    let _guard = COUNTER.lock().unwrap_or_else(|e| e.into_inner());
    let tmp = TempDir::new().unwrap();
    record_refusal(tmp.path(), "mur", "before the mark");
    let mark = refusal_count();
    assert_eq!(refusals_since(mark), 0, "a fresh mark starts at zero");

    record_refusal(tmp.path(), "mur", "after the mark");
    assert_eq!(refusals_since(mark), 1);

    // A stale/bogus mark must not panic in a reporting path.
    assert_eq!(refusals_since(usize::MAX), 0);
}

#[test]
fn unsigned_when_identity_absent() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    // Pinned to the non-enforcing policy: this is the bootstrap path, and
    // under enforcement the same call is a refusal (covered separately in
    // `missing_key_tests`).
    let ev = append_as_writer_with(
        &svc,
        tmp.path(),
        &ch.id,
        "mur",
        ChannelActor::System,
        EventKind::Note,
        serde_json::json!({ "text": "hi" }),
        None,
        false,
    )
    .unwrap();
    assert!(ev.sig.is_none(), "no identity → unsigned (migration-safe)");
}

#[test]
fn signed_when_identity_present_and_verifies() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    // Plant the router identity under <home>/agents/mur/.
    let agent_home = tmp.path().join("agents").join("mur");
    std::fs::create_dir_all(&agent_home).unwrap();
    let id = AgentIdentity::generate();
    id.save(&agent_home).unwrap();

    let ev = append_as_writer(
        &svc,
        tmp.path(),
        &ch.id,
        "mur",
        ChannelActor::System,
        EventKind::HitlResponse,
        serde_json::json!({ "allow": true }),
        None,
    )
    .unwrap();
    assert!(ev.sig.is_some(), "identity present → signed");
    let pubkey = id.verifying_key_bytes();
    assert!(
        mur_channel::sign::verify_one(&ch.id, &ev, &pubkey, true),
        "signed event must verify against the router pubkey even when require_sig"
    );
    // A key read from disk means this process is outside the agent
    // sandbox (keys/ is denied inside it), so it is the trusted writer
    // and must advance the chain-head anchor to the line it just wrote.
    let head = mur_channel::head_io::read_head(&mur_channel::anchor::heads_dir(tmp.path()), &ch.id)
        .unwrap()
        .expect("disk-keyed writer seals the head");
    assert_eq!(head.seq, ev.seq);
    assert!(head.verify(&pubkey), "head signed by the router key");
    assert!(
        !mur_channel::anchor::is_dirty(tmp.path(), &ch.id),
        "a successful anchored append leaves no dirty marker"
    );
}

/// A run's state transitions are written by the same trusted writer as
/// its messages, so a disk-keyed transition must leave the head on the
/// log instead of one line behind it.
#[test]
fn a_disk_keyed_transition_advances_the_head() {
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path().join("agents").join("mur");
    std::fs::create_dir_all(&agent_home).unwrap();
    AgentIdentity::generate().save(&agent_home).unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();

    transition_as_writer(
        &svc,
        tmp.path(),
        &ch.id,
        "mur",
        mur_common::channel::ChannelState::Completed,
        ChannelActor::System,
        None,
    )
    .unwrap();

    let log = std::fs::read(
        tmp.path()
            .join("channels")
            .join(&ch.id)
            .join("events.jsonl"),
    )
    .unwrap();
    let head = mur_channel::head_io::read_head(&mur_channel::anchor::heads_dir(tmp.path()), &ch.id)
        .unwrap()
        .expect("disk-keyed transition seals the head");
    assert_eq!(head.lines, mur_channel::head::chain_log(&log).lines);
    assert!(!mur_channel::anchor::is_dirty(tmp.path(), &ch.id));
}

/// The handed-over key is the sealed-child path: that process may append
/// (its grant covers `channels/`) but must never touch `channel-heads/`.
/// Anchoring from there would fail at `mark_dirty` BEFORE the append and
/// lose the event — so the handoff path appends without anchoring.
#[test]
fn a_handed_over_key_signs_but_does_not_anchor() {
    let tmp = TempDir::new().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    let id = AgentIdentity::generate();
    let key = WriterKey {
        id,
        kv: 1,
        source: KeySource::Handoff,
    };
    let ev = append_with_key(
        &svc,
        &ch.id,
        key,
        tmp.path(),
        ChannelActor::System,
        EventKind::HitlResponse,
        serde_json::json!({ "allow": true }),
        None,
    )
    .unwrap();
    assert!(ev.sig.is_some(), "handoff key → still signed");
    assert!(
        !mur_channel::anchor::heads_dir(tmp.path()).exists(),
        "handoff path must not create a head"
    );
    assert!(
        !mur_channel::anchor::is_dirty(tmp.path(), &ch.id),
        "handoff path must not even touch channel-heads/"
    );
}
/// #2116 regression: the daemon launches a fleet whose router is NOT the
/// agent that asked for it, from outside every sandbox and with no stdin
/// handoff. Its writes must be signed by the ROUTER (read from disk) and
/// verify under signature enforcement — never fall to "writing UNSIGNED".
#[test]
fn a_cross_router_run_outside_the_seal_signs_as_the_router() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let router_home = home.join("agents").join("orchestrator");
    std::fs::create_dir_all(&router_home).unwrap();
    let router = AgentIdentity::generate();
    router.save(&router_home).unwrap();
    // The requester has a key too; it must NOT be the one that signs.
    let requester_home = home.join("agents").join("mur");
    std::fs::create_dir_all(&requester_home).unwrap();
    AgentIdentity::generate().save(&requester_home).unwrap();

    let key = writer_key(home, "orchestrator")
        .unwrap()
        .expect("router key");
    assert!(matches!(key.source, KeySource::Disk), "no handoff in play");
    assert_eq!(
        key.id.verifying_key_bytes(),
        router.verifying_key_bytes(),
        "signed by the router, not the requester"
    );

    let svc = ChannelService::open(home).unwrap();
    let ch = svc.create_for_workflow("council").unwrap();
    let actor = ChannelActor::Agent {
        id: "orchestrator".into(),
    };
    let ev = append_as_writer(
        &svc,
        home,
        &ch.id,
        "orchestrator",
        actor,
        EventKind::Message,
        serde_json::json!({"text": "verdict"}),
        None,
    )
    .unwrap();
    assert!(ev.sig.is_some(), "a cross-router event must be signed");
    assert!(
        crate::channel_verify::verify_event(home, &ch.id, &ev, true),
        "and verify with MUR_CHANNEL_REQUIRE_SIG semantics"
    );
}
