//! at-mention C3: `mur agent send '@agent#N' <text>` addressing (design §3.4,
//! §3.5, §7). Everything up to the dial: resolution, the `#N` lookup, the
//! refusals that must write nothing, and the Human event a continued
//! conversation appends. The dial itself is today's `message/send`.

use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, EventKind};
use mur_core::cmd::agent::cli::mention_target::{TargetError, prepare_send};
use std::path::Path;
use tempfile::TempDir;

/// A home with `agents/<n>/profile.yaml` for each name — what
/// `agent_dir_names` lists — and nothing else.
fn home_with_agents(names: &[&str]) -> TempDir {
    let tmp = TempDir::new().unwrap();
    for n in names {
        let dir = tmp.path().join("agents").join(n);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("profile.yaml"), format!("name: {n}\n")).unwrap();
    }
    tmp
}

/// `prepare_send` refuses an agent with no `running.lock` before it writes;
/// the content is never read here, only its existence.
fn mark_running(home: &Path, agent: &str) {
    std::fs::write(home.join("agents").join(agent).join("running.lock"), "{}").unwrap();
}

fn ordinal(svc: &ChannelService, id: &str) -> u64 {
    svc.index().ordinal_of(id).unwrap().expect("indexed")
}

fn channel_dirs(home: &Path) -> usize {
    std::fs::read_dir(home.join("channels"))
        .map(|d| d.flatten().filter(|e| e.path().is_dir()).count())
        .unwrap_or(0)
}

#[test]
fn send_at_agent_ordinal_continues_channel() {
    let tmp = home_with_agents(&["a", "b"]);
    let home = tmp.path();
    mark_running(home, "a");
    let svc = ChannelService::open(home).unwrap();
    let ch = svc.create_for_agent("a").unwrap();
    let n = ordinal(&svc, &ch.id);

    let p = prepare_send(home, &format!("@a#{n}"), "continue please").unwrap();

    assert_eq!(p.agent, "a");
    assert_eq!(
        p.channel_id, ch.id,
        "must continue channel #{n}, not mint one"
    );
    let ev = svc
        .load_events(&ch.id)
        .unwrap()
        .into_iter()
        .find(|e| e.seq == p.channel_seq)
        .expect("the Human event at channel_seq");
    assert!(matches!(ev.actor, ChannelActor::Human { .. }));
    assert_eq!(ev.kind, EventKind::Message);
    assert_eq!(ev.payload["text"], "continue please");
    assert_eq!(p.params["context"]["channel_id"], ch.id.as_str());
    assert_eq!(p.params["context"]["channel_seq"], p.channel_seq);
    assert_eq!(p.params["can_approve"], false);
    assert_eq!(
        p.params["message"]["parts"][0]["text"], "continue please",
        "the turn input must equal the event text or the watermark refuses it"
    );
}

#[test]
fn send_at_agent_without_ordinal_continues_latest_conversation() {
    let tmp = home_with_agents(&["a"]);
    let home = tmp.path();
    mark_running(home, "a");
    let svc = ChannelService::open(home).unwrap();
    let ch = svc.create_for_agent("a").unwrap();

    let p = prepare_send(home, "@A", "hi").unwrap();

    assert_eq!(p.agent, "a", "agent names resolve case-insensitively");
    assert_eq!(p.channel_id, ch.id);
}

#[test]
fn send_at_agent_ordinal_zero_is_unknown_conversation() {
    let tmp = home_with_agents(&["a"]);
    mark_running(tmp.path(), "a");
    let e = prepare_send(tmp.path(), "@a#0", "hi").unwrap_err();
    assert!(matches!(e, TargetError::UnknownConversation(0)), "{e}");
    assert!(e.to_string().starts_with("UNKNOWN_CONVERSATION"), "{e}");
}

#[test]
fn send_at_agent_unknown_ordinal_creates_nothing() {
    let tmp = home_with_agents(&["a"]);
    let home = tmp.path();
    mark_running(home, "a");
    let before = channel_dirs(home);

    let e = prepare_send(home, "@a#99", "hi").unwrap_err();

    assert!(matches!(e, TargetError::UnknownConversation(99)), "{e}");
    assert_eq!(
        channel_dirs(home),
        before,
        "a refused send writes no channel"
    );
}

#[test]
fn send_at_agent_non_member_adds_no_participant() {
    let tmp = home_with_agents(&["a", "b"]);
    let home = tmp.path();
    mark_running(home, "a");
    let svc = ChannelService::open(home).unwrap();
    let ch = svc.create_for_agent("b").unwrap();
    let n = ordinal(&svc, &ch.id);
    let events_before = svc.load_events(&ch.id).unwrap().len();

    let e = prepare_send(home, &format!("@a#{n}"), "hi").unwrap_err();

    assert!(matches!(e, TargetError::NotAParticipant { .. }), "{e}");
    assert!(e.to_string().starts_with("NOT_A_PARTICIPANT"), "{e}");
    let after = svc.store().load_manifest(&ch.id).unwrap();
    let agents = |c: &mur_common::channel::Channel| -> Vec<String> {
        c.participants
            .iter()
            .filter_map(|p| match &p.actor {
                ChannelActor::Agent { id } => Some(id.clone()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(agents(&after), agents(&ch), "no participant added");
    assert_eq!(svc.load_events(&ch.id).unwrap().len(), events_before);
}

#[test]
fn send_at_workflow_titled_channel_is_not_conversation() {
    let tmp = home_with_agents(&["a"]);
    let home = tmp.path();
    mark_running(home, "a");
    let svc = ChannelService::open(home).unwrap();
    let ch = svc.create_for_workflow("nightly").unwrap();
    let n = ordinal(&svc, &ch.id);

    let e = prepare_send(home, &format!("@a#{n}"), "hi").unwrap_err();

    assert!(
        matches!(e, TargetError::ChannelNotConversation(m) if m == n),
        "{e}"
    );
    assert!(e.to_string().starts_with("CHANNEL_NOT_CONVERSATION"), "{e}");
}

#[test]
fn name_in_both_namespaces_is_ambiguous() {
    let tmp = home_with_agents(&["ops"]);
    let home = tmp.path();
    let fleet = home.join("fleets").join("ops");
    std::fs::create_dir_all(&fleet).unwrap();
    std::fs::write(fleet.join("fleet.yaml"), "name: ops\n").unwrap();
    mark_running(home, "ops");

    let e = prepare_send(home, "@ops", "hi").unwrap_err();
    assert!(matches!(e, TargetError::AmbiguousTarget(_)), "{e}");
    assert!(e.to_string().starts_with("AMBIGUOUS_TARGET"), "{e}");
    assert_eq!(channel_dirs(home), 0);

    // The prefix is the way out, and a fleet is not deliverable in Phase 1.
    let e = prepare_send(home, "@fleet:ops", "hi").unwrap_err();
    assert!(matches!(e, TargetError::FleetNotSupported(_)), "{e}");
}

#[test]
fn send_to_a_stopped_agent_writes_nothing() {
    let tmp = home_with_agents(&["a"]);
    let home = tmp.path();
    let e = prepare_send(home, "@a", "hi").unwrap_err();
    assert!(matches!(e, TargetError::TargetOffline(_)), "{e}");
    assert_eq!(
        channel_dirs(home),
        0,
        "no orphan Human event for a dead agent"
    );
}
