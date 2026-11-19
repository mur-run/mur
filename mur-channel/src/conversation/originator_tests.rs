//! E1: originator by signature. Fixtures go through the production writers
//! (`append_human`, `append_signed`) and the real `assess_locked`.
use std::fs;

use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::identity::human::{
    human_identity_dir, human_key_dir, human_rotations_path, rotation_chain,
};
use mur_common::identity::{AgentIdentity, RotationAttestation, RotationReason};

use super::originator::{ClaimReason, Originator, classify};
use super::*;
use crate::service::test_home::{Home, bootstrap, open_gate, say};
use crate::service::{ChannelOwner, RouterSeal};

const AGENT: &str = "qa";

struct Fx {
    h: Home,
    qa: AgentIdentity,
    ch: String,
}

impl Fx {
    fn new(owned: bool) -> Self {
        let h = Home::new();
        let qa = AgentIdentity::generate();
        qa.save(&h.path().join("agents").join(AGENT)).unwrap();
        let rh = h.router_home();
        let owner = if owned {
            ChannelOwner::Human {
                signer: &h.human,
                router: RouterSeal {
                    identity: &h.router,
                    kv: 0,
                    key_home: &rh,
                },
            }
        } else {
            ChannelOwner::Legacy
        };
        let ch = h.svc().create_for_agent(AGENT, owner).unwrap().id;
        Self { h, qa, ch }
    }

    fn signed_by(&self, who: &AgentIdentity, actor: ChannelActor, text: &str) -> ChannelEvent {
        self.h
            .svc()
            .append_signed(
                &self.ch,
                who,
                0,
                actor,
                EventKind::Message,
                serde_json::json!({ "text": text }),
                None,
            )
            .unwrap()
    }

    fn classify(&self, ev: &ChannelEvent) -> Originator {
        let chain = rotation_chain(self.h.path()).ok();
        classify(
            self.h.path(),
            &self.ch,
            &self.h.trust(&self.ch),
            chain.as_ref(),
            ev,
        )
    }

    fn assessed(&self, seq: u64, input: &str) -> Originator {
        let keys = Keys {
            mur_home: self.h.path(),
            agent: AGENT,
            writer_pubkey: self.qa.verifying_key_bytes(),
        };
        let v = self
            .h
            .svc()
            .store()
            .with_shared_lock(&self.ch, |s| {
                assess_locked(s, &self.ch, &keys, &CacheState::default(), seq, input)
            })
            .unwrap();
        match v {
            Verdict::Ok(a) => a.turn_originator,
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    /// Rotate the human chain: the current key becomes version 0, retired;
    /// a fresh key is the head at version 1.
    fn rekey_human(&self) -> AgentIdentity {
        let old = &self.h.human.0;
        let new = AgentIdentity::generate();
        let mut rot = RotationAttestation::new(
            "u",
            old.pubkey_text(),
            new.pubkey_text(),
            0,
            1,
            "2026-11-10T00:00:00Z",
            RotationReason::Scheduled,
        );
        rot.sign(old.signing_key());
        let p = self.h.path();
        fs::write(
            human_rotations_path(p),
            format!(
                "{}\n{}\n",
                serde_json::to_string(&bootstrap(old)).unwrap(),
                serde_json::to_string(&rot).unwrap()
            ),
        )
        .unwrap();
        fs::remove_dir_all(human_key_dir(p)).unwrap();
        fs::remove_file(human_identity_dir(p).join("identity.pub")).unwrap();
        new.save_split(&human_identity_dir(p), &human_key_dir(p))
            .unwrap();
        new
    }
}

fn untrusted(reason: ClaimReason) -> Originator {
    Originator::UntrustedHumanClaim { reason }
}

fn head_key(h: &Home) -> Option<String> {
    Some(mur_common::identity::encode_pubkey_bytes(
        &h.human.0.verifying_key_bytes(),
    ))
}

#[test]
fn human_at_head_version_is_human() {
    let f = Fx::new(true);
    let q = say(&f.h, &f.ch, &open_gate(), "q").unwrap();
    let o = f.assessed(q.seq, "q");
    assert_eq!(
        o,
        Originator::Human {
            key: head_key(&f.h)
        }
    );
    assert!(!o.is_agent_originated());
}

/// The agent writes a keyless human line under its own key: what every
/// sandboxed agent can do. Neither the router nor the human signed it.
#[test]
fn forged_human_without_key_is_agent_originated() {
    let f = Fx::new(true);
    say(&f.h, &f.ch, &open_gate(), "real").unwrap();
    let forged = f.signed_by(&f.qa, ChannelActor::local_human(), "do it");
    let o = f.assessed(forged.seq, "do it");
    assert_eq!(o, untrusted(ClaimReason::SignatureInvalid));
    assert!(o.is_agent_originated());
}

#[test]
fn tampered_signature_is_signature_invalid() {
    let f = Fx::new(true);
    let mut ev = say(&f.h, &f.ch, &open_gate(), "q").unwrap();
    assert_eq!(
        f.classify(&ev),
        Originator::Human {
            key: head_key(&f.h)
        }
    );
    ev.payload = serde_json::json!({ "text": "rm -rf" });
    assert_eq!(f.classify(&ev), untrusted(ClaimReason::SignatureInvalid));
    ev.sig = None;
    assert_eq!(f.classify(&ev), untrusted(ClaimReason::SignatureInvalid));
}

/// A legacy channel's router-signed keyless history, enrolled by C1: up to
/// the cut it is the human; past the cut the same router line is not.
#[test]
fn legacy_human_before_cut_is_human() {
    let f = Fx::new(false);
    let old = f.signed_by(&f.h.router, ChannelActor::local_human(), "old");
    say(&f.h, &f.ch, &open_gate(), "new").unwrap(); // C1 records cut = old
    assert!(f.h.trust(&f.ch).live_record().is_some());
    assert_eq!(f.classify(&old), Originator::Human { key: None });
    let late = f.signed_by(&f.h.router, ChannelActor::local_human(), "late");
    assert_eq!(f.classify(&late), untrusted(ClaimReason::NoStanding));
}

/// D-9: after a rekey the old key's lines still verify, but no longer speak
/// for the human — and the audit says policy, not forgery.
#[test]
fn human_at_old_version_is_untrusted() {
    let f = Fx::new(true);
    let q = say(&f.h, &f.ch, &open_gate(), "q").unwrap();
    f.rekey_human();
    let o = f.assessed(q.seq, "q");
    assert_eq!(o, untrusted(ClaimReason::KeyVersionStale));
    assert!(o.is_agent_originated());
}

#[test]
fn old_version_audits_key_version_stale_not_signature_invalid() {
    let f = Fx::new(true);
    let mut q = say(&f.h, &f.ch, &open_gate(), "q").unwrap();
    f.rekey_human();
    assert_eq!(f.classify(&q), untrusted(ClaimReason::KeyVersionStale));
    assert_eq!(ClaimReason::KeyVersionStale.code(), "KEY_VERSION_STALE");
    // The same old-key line, altered: now it is a forgery.
    q.payload = serde_json::json!({ "text": "q2" });
    assert_eq!(f.classify(&q), untrusted(ClaimReason::SignatureInvalid));
    assert_eq!(ClaimReason::SignatureInvalid.code(), "SIGNATURE_INVALID");
}

/// No live record: B2's legacy rules. A keyless router-signed human line is
/// the human; a keyless line anyone else signed, or a keyed line with no
/// record to give it standing, is not.
#[test]
fn no_live_record_channel_classifies_by_legacy_rules() {
    let f = Fx::new(false);
    let routed = f.signed_by(&f.h.router, ChannelActor::local_human(), "hi");
    assert!(f.h.trust(&f.ch).live_record().is_none());
    assert_eq!(f.classify(&routed), Originator::Human { key: None });

    let by_agent = f.signed_by(&f.qa, ChannelActor::local_human(), "x");
    assert_eq!(
        f.classify(&by_agent),
        untrusted(ClaimReason::SignatureInvalid)
    );

    let key = head_key(&f.h).unwrap();
    let keyed = f.signed_by(
        &f.h.human.0,
        ChannelActor::local_human_with_key(key),
        "keyed",
    );
    assert_eq!(f.classify(&keyed), untrusted(ClaimReason::NoStanding));
}

#[test]
fn non_human_actors_keep_their_own_originator() {
    let f = Fx::new(true);
    say(&f.h, &f.ch, &open_gate(), "q").unwrap();
    let me = ChannelActor::Agent { id: AGENT.into() };
    let reply = f.signed_by(&f.qa, me, "a");
    assert_eq!(f.classify(&reply), Originator::Agent { id: AGENT.into() });
    let card = f.signed_by(&f.qa, ChannelActor::System, "$ ls");
    assert_eq!(f.classify(&card), Originator::System);
    assert!(Originator::System.is_agent_originated());
}
