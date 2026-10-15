//! Intake against real enrollment records: a request for an enrolled remote reaches
//! `pending_approval` with the epoch and repo identity the records carry, an approval pushes to
//! the enrolled `file://` remote, and a revoke or an unreadable file refuses cleanly (#1764, 2a).
#![cfg(unix)]

use chrono::Utc;
use mur_common::git_push::{enrollments_path, inbox_dir, status_dir};
use mur_common::hitl::approval_token::SECRETS_DIR;
use mur_common::identity::AgentIdentity;
use mur_daemon::git_push_broker::{BrokerSlot, sweep_with};
use mur_git_broker::action::ActionDocument;
use mur_git_broker::approval::ApprovalProof;
use mur_git_broker::credential::{Credential, SecretPath, Username};
use mur_git_broker::enrollment::{Endpoint, EnrollRequest, Enrollments, NewRepo};
use mur_git_broker::oid::ObjectFormat;
use mur_git_broker::pending::{RequestKey, State};
use mur_git_broker::request_file::{RequestFile, pack_path, sha256_file, write_request};
use std::path::{Path, PathBuf};
use std::process::Command;

const AGENT: &str = "bob";
const REPO: &str = "mur";
const REMOTE: &str = "origin";
const TASK: &str = "t1";
const PREFIX: &str = "refs/heads/agent/bob/";
const BRANCH: &str = "refs/heads/agent/bob/x";

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap().trim().to_string()
}

struct World {
    home: tempfile::TempDir,
    id: AgentIdentity,
    work: PathBuf,
    remote: PathBuf,
    old: String,
    new: String,
}

fn world() -> World {
    let home = tempfile::tempdir().unwrap();
    let id = AgentIdentity::generate();
    id.save(&home.path().join("agents").join(AGENT)).unwrap();
    std::fs::write(
        home.path().join("config.yaml"),
        format!("git_push:\n  enabled: true\n  agents:\n    - {AGENT}\n"),
    )
    .unwrap();
    let work = home.path().join("work");
    let remote = home.path().join("remote.git");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["commit", "-q", "--allow-empty", "-m", "old"]);
    let old = git(&work, &["rev-parse", "HEAD"]);
    git(
        home.path(),
        &["init", "-q", "--bare", remote.to_str().unwrap()],
    );
    git(
        &work,
        &[
            "push",
            "-q",
            remote.to_str().unwrap(),
            &format!("{old}:{BRANCH}"),
        ],
    );
    git(&work, &["commit", "-q", "--allow-empty", "-m", "new"]);
    let new = git(&work, &["rev-parse", "HEAD"]);
    World {
        home,
        id,
        work,
        remote,
        old,
        new,
    }
}

fn enroll(w: &World) {
    enroll_with(w, None);
}

fn enroll_with(w: &World, credential: Option<Credential>) {
    let mut e = Enrollments::empty(100);
    e.enroll(EnrollRequest {
        agent: AGENT.into(),
        repo_id: REPO.into(),
        remote_id: REMOTE.into(),
        endpoint: Endpoint::parse(&format!("file://{}", w.remote.display())).unwrap(),
        ref_prefix: PREFIX.into(),
        creation_base_refs: vec![],
        credential,
        new_repo: Some(NewRepo {
            path: w.work.clone(),
            object_format: ObjectFormat::Sha1,
        }),
    })
    .unwrap();
    e.save(w.home.path()).unwrap();
}

fn pack(w: &World) -> PathBuf {
    let out = w.work.join("r.pack");
    let mut c = Command::new("git")
        .current_dir(&w.work)
        .args(["pack-objects", "--revs", "--stdout"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    c.stdin
        .take()
        .unwrap()
        .write_all(format!("{}\n^{}\n", w.new, w.old).as_bytes())
        .unwrap();
    let o = c.wait_with_output().unwrap();
    assert!(o.status.success());
    std::fs::write(&out, o.stdout).unwrap();
    out
}

fn drop_request(w: &World, request_id: &str, remote_id: &str) {
    let p = pack(w);
    let mut r = RequestFile {
        agent: AGENT.into(),
        task_id: TASK.into(),
        request_id: request_id.into(),
        repo_id: REPO.into(),
        remote_id: remote_id.into(),
        r#ref: BRANCH.into(),
        old_sha: w.old.clone(),
        new_sha: w.new.clone(),
        pack_sha256: sha256_file(&p).unwrap(),
        requested_at: Utc::now(),
        key_version: 0,
        sig: String::new(),
    };
    r.sig = w.id.sign_multibase(&r.sign_input());
    let inbox = inbox_dir(&w.home.path().join("agents").join(AGENT));
    write_request(&inbox, &r).unwrap();
    std::fs::copy(&p, pack_path(&inbox, request_id)).unwrap();
}

fn status(w: &World, request_id: &str) -> String {
    std::fs::read_to_string(status_dir(w.home.path(), AGENT).join(format!("{request_id}.yaml")))
        .unwrap_or_default()
}

fn key(id: &str) -> RequestKey {
    RequestKey {
        agent_id: AGENT.into(),
        task_id: TASK.into(),
        request_id: id.into(),
    }
}

#[test]
fn enrolled_request_pends_stamped_with_the_records_and_an_approval_pushes() {
    let w = world();
    enroll(&w);
    let recs = Enrollments::load(&enrollments_path(w.home.path()))
        .unwrap()
        .unwrap();
    let slot = BrokerSlot::default();
    drop_request(&w, "r1", REMOTE);
    sweep_with(w.home.path(), &slot);

    let s = status(&w, "r1");
    assert!(s.contains("state: pending_approval"), "{s}");
    let broker = slot.get(w.home.path()).unwrap();
    let row = broker.store().get(&key("r1")).unwrap().unwrap();
    assert_eq!(row.state, State::PendingApproval);
    let doc: ActionDocument = serde_json::from_str(&row.doc_json).unwrap();
    assert_eq!(
        doc.enrollment_epoch, recs.enrollments[0].epoch,
        "epoch is the record's"
    );
    assert_eq!(
        doc.repo_identity, recs.repos[REPO].identity,
        "identity is the record's"
    );
    assert!(
        s.contains(&row.action_hash),
        "the status names the hash a human approves: {s}"
    );

    let proof = ApprovalProof {
        event_id: "e1".into(),
        request_id: "r1".into(),
        action_hash: row.action_hash.clone(),
    };
    assert_eq!(
        broker.on_approval(&key("r1"), proof).unwrap(),
        State::Succeeded
    );
    assert_eq!(git(&w.remote, &["rev-parse", BRANCH]), w.new);
}

#[test]
fn request_for_a_remote_nobody_enrolled_is_not_enrolled_and_never_opens_the_broker() {
    let w = world();
    enroll(&w);
    drop_request(&w, "r2", "elsewhere");
    sweep_with(w.home.path(), &BrokerSlot::default());
    assert!(
        status(&w, "r2").contains("code: not_enrolled"),
        "{}",
        status(&w, "r2")
    );
    assert!(
        !w.home
            .path()
            .join("git-push/private/pending.sqlite")
            .exists()
    );
}

#[test]
fn no_enrollments_file_is_not_enrolled() {
    let w = world();
    drop_request(&w, "r3", REMOTE);
    sweep_with(w.home.path(), &BrokerSlot::default());
    assert!(status(&w, "r3").contains("code: not_enrolled"));
}

#[test]
fn a_corrupt_enrollments_file_refuses_the_request_as_unavailable_not_as_not_enrolled() {
    let w = world();
    enroll(&w);
    std::fs::write(enrollments_path(w.home.path()), "version: [not, a, record").unwrap();
    drop_request(&w, "r4", REMOTE);
    sweep_with(w.home.path(), &BrokerSlot::default());
    let s = status(&w, "r4");
    assert!(s.contains("code: enrollment_unavailable"), "{s}");
    assert!(
        !w.home
            .path()
            .join("git-push/private/pending.sqlite")
            .exists()
    );
}

#[test]
fn a_revoke_applies_to_the_next_request_with_the_same_running_broker() {
    let w = world();
    enroll(&w);
    let slot = BrokerSlot::default();
    drop_request(&w, "r5", REMOTE);
    sweep_with(w.home.path(), &slot);
    assert!(status(&w, "r5").contains("state: pending_approval"));

    let path = enrollments_path(w.home.path());
    let mut e = Enrollments::load(&path).unwrap().unwrap();
    assert!(e.revoke(AGENT, REPO, REMOTE));
    e.save(w.home.path()).unwrap();

    drop_request(&w, "r6", REMOTE);
    sweep_with(w.home.path(), &slot);
    assert!(
        status(&w, "r6").contains("code: not_enrolled"),
        "{}",
        status(&w, "r6")
    );
    // And the request that was already pending is void the moment someone approves it.
    let broker = slot.get(w.home.path()).unwrap();
    let hash = broker.store().get(&key("r5")).unwrap().unwrap().action_hash;
    let proof = ApprovalProof {
        event_id: "e1".into(),
        request_id: "r5".into(),
        action_hash: hash,
    };
    assert!(broker.on_approval(&key("r5"), proof).is_err());
    assert_eq!(
        git(&w.remote, &["rev-parse", BRANCH]),
        w.old,
        "nothing pushed"
    );
}

#[test]
fn a_pack_that_does_not_match_the_signed_digest_is_refused() {
    let w = world();
    enroll(&w);
    drop_request(&w, "r7", REMOTE);
    let inbox = inbox_dir(&w.home.path().join("agents").join(AGENT));
    std::fs::write(pack_path(&inbox, "r7"), b"PACK-but-not-the-signed-one").unwrap();
    sweep_with(w.home.path(), &BrokerSlot::default());
    assert!(
        status(&w, "r7").contains("code: invalid_request"),
        "{}",
        status(&w, "r7")
    );
}

// ---- the approval loop: a real channel, real signatures, no hand-made `ApprovalProof` ----

use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};
use mur_core::channel_writer::{ROUTER_AGENT, append_as_writer};
use mur_daemon::git_push_broker::{BROKER_CHANNEL_ID, approval_pass};

/// The router identity every HITL event is signed with. Without it the request is unsigned and
/// nothing can bind to it.
fn plant_router(w: &World) {
    AgentIdentity::generate()
        .save(&w.home.path().join("agents").join(ROUTER_AGENT))
        .unwrap();
}

fn answer(w: &World, hitl_id: &str, hash: &str, allow: bool, actor: ChannelActor) {
    let svc = ChannelService::open(w.home.path()).unwrap();
    append_as_writer(
        &svc,
        w.home.path(),
        BROKER_CHANNEL_ID,
        ROUTER_AGENT,
        actor,
        EventKind::HitlResponse,
        serde_json::json!({
            "hitl_id": hitl_id, "action_hash": hash, "allow": allow,
            "reason": "", "surface": "cli", "issued_at": Utc::now(),
        }),
        None,
    )
    .unwrap();
}

/// A request pending, with the router in place and one approval pass run so the question is asked.
fn asked(w: &World, id: &str, slot: &BrokerSlot) -> String {
    enroll(w);
    plant_router(w);
    drop_request(w, id, REMOTE);
    sweep_with(w.home.path(), slot);
    approval_pass(w.home.path(), slot);
    slot.get(w.home.path())
        .unwrap()
        .store()
        .get(&key(id))
        .unwrap()
        .unwrap()
        .action_hash
}

fn state_of(w: &World, slot: &BrokerSlot, id: &str) -> State {
    slot.get(w.home.path())
        .unwrap()
        .store()
        .get(&key(id))
        .unwrap()
        .unwrap()
        .state
}

#[test]
fn a_pending_row_is_asked_once_with_its_own_hash_and_waits_without_an_answer() {
    let w = world();
    let slot = BrokerSlot::default();
    let hash = asked(&w, "r1", &slot);
    approval_pass(w.home.path(), &slot);
    approval_pass(w.home.path(), &slot);

    let evs = ChannelService::open(w.home.path())
        .unwrap()
        .load_events(BROKER_CHANNEL_ID)
        .unwrap();
    let reqs: Vec<_> = evs
        .iter()
        .filter(|e| e.kind == EventKind::HitlRequest)
        .collect();
    assert_eq!(reqs.len(), 1, "asked once however many passes run");
    assert_eq!(reqs[0].payload["hitl_id"], "r1");
    assert_eq!(reqs[0].payload["action_hash"], hash.as_str());
    assert!(reqs[0].sig.is_some(), "the router signs the question");
    assert_eq!(state_of(&w, &slot, "r1"), State::PendingApproval);
    assert_eq!(
        git(&w.remote, &["rev-parse", BRANCH]),
        w.old,
        "nothing pushed"
    );
}

#[test]
fn a_human_allow_on_the_channel_pushes_and_the_status_says_succeeded() {
    let w = world();
    let slot = BrokerSlot::default();
    let hash = asked(&w, "r1", &slot);
    answer(&w, "r1", &hash, true, ChannelActor::local_human());
    approval_pass(w.home.path(), &slot);

    assert_eq!(state_of(&w, &slot, "r1"), State::Succeeded);
    assert_eq!(git(&w.remote, &["rev-parse", BRANCH]), w.new);
    assert!(
        status(&w, "r1").contains("state: succeeded"),
        "{}",
        status(&w, "r1")
    );
    // A replay of the same log is inert: the row is terminal and the tombstone holds.
    approval_pass(w.home.path(), &slot);
    assert_eq!(state_of(&w, &slot, "r1"), State::Succeeded);
}

#[test]
fn a_human_deny_ends_the_row_denied_and_pushes_nothing() {
    let w = world();
    let slot = BrokerSlot::default();
    let hash = asked(&w, "r1", &slot);
    answer(&w, "r1", &hash, false, ChannelActor::local_human());
    approval_pass(w.home.path(), &slot);

    assert_eq!(state_of(&w, &slot, "r1"), State::Denied);
    assert_eq!(git(&w.remote, &["rev-parse", BRANCH]), w.old);
    assert!(
        status(&w, "r1").contains("state: denied"),
        "{}",
        status(&w, "r1")
    );
}

#[test]
fn an_agents_own_allow_and_a_wrong_hash_release_nothing() {
    let w = world();
    let slot = BrokerSlot::default();
    let hash = asked(&w, "r1", &slot);
    // The agent answering as itself, correctly signed with the router key it cannot hold in
    // production; the actor is what makes it not the human's.
    answer(
        &w,
        "r1",
        &hash,
        true,
        ChannelActor::Agent { id: AGENT.into() },
    );
    // The human's allow, but for a different action.
    answer(&w, "r1", &"0".repeat(64), true, ChannelActor::local_human());
    // The human's allow for a request nobody asked.
    answer(&w, "ghost", &hash, true, ChannelActor::local_human());
    approval_pass(w.home.path(), &slot);

    assert_eq!(state_of(&w, &slot, "r1"), State::PendingApproval);
    assert_eq!(git(&w.remote, &["rev-parse", BRANCH]), w.old);
}

#[test]
fn an_allow_after_the_enrollment_was_revoked_ends_policy_changed_and_pushes_nothing() {
    let w = world();
    let slot = BrokerSlot::default();
    let hash = asked(&w, "r1", &slot);
    std::fs::remove_file(enrollments_path(w.home.path())).unwrap();
    answer(&w, "r1", &hash, true, ChannelActor::local_human());
    approval_pass(w.home.path(), &slot);

    assert_eq!(state_of(&w, &slot, "r1"), State::PolicyChanged);
    assert_eq!(git(&w.remote, &["rev-parse", BRANCH]), w.old);
    assert!(
        status(&w, "r1").contains("state: policy_changed"),
        "{}",
        status(&w, "r1")
    );
}

#[test]
fn an_allow_after_a_revoke_and_re_enroll_ends_policy_changed_and_pushes_nothing() {
    let w = world();
    let slot = BrokerSlot::default();
    let hash = asked(&w, "r1", &slot);
    // Same policy, same repo: only the epoch moves. The human approved the old enrollment.
    let mut book = Enrollments::load(&enrollments_path(w.home.path()))
        .unwrap()
        .unwrap();
    let old = book.enrollments[0].epoch;
    let rec = book.enrollments.remove(0);
    book.enroll(EnrollRequest {
        agent: rec.agent,
        repo_id: rec.repo_id,
        remote_id: rec.remote_id,
        endpoint: rec.endpoint,
        ref_prefix: rec.ref_prefix,
        creation_base_refs: rec.creation_base_refs,
        credential: rec.credential,
        new_repo: None,
    })
    .unwrap();
    assert!(
        book.enrollments[0].epoch > old,
        "re-enroll must mint a new epoch"
    );
    book.save(w.home.path()).unwrap();

    answer(&w, "r1", &hash, true, ChannelActor::local_human());
    approval_pass(w.home.path(), &slot);

    assert_eq!(state_of(&w, &slot, "r1"), State::PolicyChanged);
    assert_eq!(git(&w.remote, &["rev-parse", BRANCH]), w.old);
}

#[test]
fn an_unreadable_enrollment_file_leaves_the_row_pending_and_the_approval_unspent() {
    let w = world();
    let slot = BrokerSlot::default();
    let hash = asked(&w, "r1", &slot);
    let good = std::fs::read(enrollments_path(w.home.path())).unwrap();
    std::fs::write(enrollments_path(w.home.path()), b"{ not: [valid").unwrap();
    answer(&w, "r1", &hash, true, ChannelActor::local_human());
    approval_pass(w.home.path(), &slot);
    assert_eq!(state_of(&w, &slot, "r1"), State::PendingApproval);

    // The file comes back; the same standing approval now goes through.
    std::fs::write(enrollments_path(w.home.path()), good).unwrap();
    approval_pass(w.home.path(), &slot);
    assert_eq!(state_of(&w, &slot, "r1"), State::Succeeded);
    assert_eq!(git(&w.remote, &["rev-parse", BRANCH]), w.new);
}

/// Writes `<home>/secrets/<name>` with `mode` (the dir is `0700`) and returns a credential that
/// points at it.
fn plant_secret(w: &World, mode: u32) -> (Credential, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let dir = w.home.path().join(SECRETS_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let file = dir.join("tok");
    std::fs::write(&file, b"ghp_not_a_real_token\n").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
    let cred = Credential {
        secret_path: SecretPath::parse(file.to_str().unwrap()).unwrap(),
        username: Username::parse("x-access-token").unwrap(),
    };
    (cred, file)
}

/// Enrolls with a credential, drops a request, sweeps it to pending, returns its proof.
fn pending_with_credential(w: &World, slot: &BrokerSlot, cred: Credential) -> ApprovalProof {
    enroll_with(w, Some(cred));
    drop_request(w, "r1", REMOTE);
    sweep_with(w.home.path(), slot);
    let row = slot
        .get(w.home.path())
        .unwrap()
        .store()
        .get(&key("r1"))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, State::PendingApproval, "{}", status(w, "r1"));
    ApprovalProof {
        event_id: "e1".into(),
        request_id: "r1".into(),
        action_hash: row.action_hash,
    }
}

#[test]
fn a_binding_with_a_readable_credential_goes_through_the_real_file_source_and_pushes() {
    let w = world();
    let (cred, _) = plant_secret(&w, 0o600);
    let slot = BrokerSlot::default();
    let proof = pending_with_credential(&w, &slot, cred);
    let broker = slot.get(w.home.path()).unwrap();
    assert_eq!(
        broker.on_approval(&key("r1"), proof).unwrap(),
        State::Succeeded
    );
    assert_eq!(git(&w.remote, &["rev-parse", BRANCH]), w.new);
}

#[test]
fn an_unusable_credential_file_keeps_the_row_pending_and_the_approval_unspent() {
    let w = world();
    // 0644 is refused by the real `FileCredentials`; this is also what proves `open` was reached.
    let (cred, file) = plant_secret(&w, 0o644);
    let slot = BrokerSlot::default();
    let proof = pending_with_credential(&w, &slot, cred);
    let broker = slot.get(w.home.path()).unwrap();

    let err = broker.on_approval(&key("r1"), proof.clone()).unwrap_err();
    assert!(
        matches!(
            err,
            mur_git_broker::error::BrokerError::CredentialUnreadable(_)
        ),
        "{err:?}"
    );
    assert_eq!(state_of_row(&broker), State::PendingApproval);
    assert_eq!(
        git(&w.remote, &["rev-parse", BRANCH]),
        w.old,
        "nothing was pushed while the credential was unusable"
    );

    // Fixed on disk; the very same proof now goes through.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        broker.on_approval(&key("r1"), proof).unwrap(),
        State::Succeeded
    );
    assert_eq!(git(&w.remote, &["rev-parse", BRANCH]), w.new);
}

fn state_of_row(broker: &mur_git_broker::broker::Broker) -> State {
    broker.store().get(&key("r1")).unwrap().unwrap().state
}
