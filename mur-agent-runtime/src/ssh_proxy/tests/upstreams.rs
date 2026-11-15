//! Two upstreams (D spec §"Proxy with two upstreams"): the user's agent and
//! the runtime-private one holding the dedicated key.

use std::collections::HashSet;

use super::fixtures::*;
use super::gate::{FakeGate, forwarded_signs};
use super::session::{FakeUpstream, World};
use crate::ssh_proxy::gate::GateOutcome;
use crate::ssh_proxy::keys::fingerprint;
use crate::ssh_proxy::policy::{PinTrust, Policy};
use crate::ssh_proxy::session::Session;
use crate::ssh_proxy::wire::Answer;
use mur_common::agent::{SshEntitlement, SshHostRule, SshKeyRule};

/// Every world key allowed on the world's host.
fn policy_all(w: &World) -> Policy {
    let host = fingerprint(&w.host.key_blob());
    Policy::build(
        &SshEntitlement {
            keys: w
                .keys
                .iter()
                .map(|k| SshKeyRule {
                    fingerprint: fingerprint(&k.key_blob()),
                    label: None,
                    allow_hosts: vec![SshHostRule {
                        host_key: host.clone(),
                        label: None,
                        known_hosts: Vec::new(),
                    }],
                })
                .collect(),
        },
        PinTrust::Pinned,
        true,
    )
}

fn blobs(w: &World, idx: &[usize]) -> Vec<Vec<u8>> {
    idx.iter().map(|i| w.keys[*i].key_blob()).collect()
}

fn listed(a: Answer) -> Vec<Vec<u8>> {
    match a {
        Answer::Identities(ids) => ids.into_iter().map(|i| i.key_blob).collect(),
        other => panic!("expected identities, got {other:?}"),
    }
}

#[tokio::test]
async fn the_list_is_dedicated_first_then_user_without_duplicates() {
    let w = World::new();
    let p = policy_all(&w);
    let ded = FakeUpstream::new(blobs(&w, &[0]));
    let user = FakeUpstream::new(blobs(&w, &[1, 0, 2]));
    let set: HashSet<Vec<u8>> = blobs(&w, &[0]).into_iter().collect();
    let sink = w.sink();
    let gate = FakeGate::new(GateOutcome::Deny);
    let mut s = Session::new(&p, &user, &sink, &*gate).with_dedicated(&ded, &set);
    let got = listed(s.handle(&[wire_list()]).await);
    assert_eq!(got, blobs(&w, &[0, 1, 2]));
}

#[tokio::test]
async fn each_key_signs_only_on_the_upstream_that_holds_it() {
    let w = World::new();
    let p = policy_all(&w);
    let ded = FakeUpstream::new(blobs(&w, &[0]));
    let user = FakeUpstream::new(blobs(&w, &[1]));
    let set: HashSet<Vec<u8>> = blobs(&w, &[0]).into_iter().collect();
    let sink = w.sink();
    let gate = FakeGate::new(GateOutcome::Allow { decision: None });
    let mut s = Session::new(&p, &user, &sink, &*gate).with_dedicated(&ded, &set);
    assert_eq!(s.handle(&w.good_bind()).await, Answer::Success);
    let a = s.handle(&sign_body(&w.keys[0].key_blob())).await;
    assert!(matches!(a, Answer::SignResponse(_)), "{a:?}");
    assert_eq!((forwarded_signs(&ded), forwarded_signs(&user)), (1, 0));
    let a = s.handle(&sign_body(&w.keys[1].key_blob())).await;
    assert!(matches!(a, Answer::SignResponse(_)), "{a:?}");
    assert_eq!((forwarded_signs(&ded), forwarded_signs(&user)), (1, 1));
}

#[tokio::test]
async fn a_dead_user_agent_does_not_hide_the_dedicated_key() {
    let w = World::new();
    let p = policy_all(&w);
    let ded = FakeUpstream::new(blobs(&w, &[0]));
    let user = FakeUpstream::new(blobs(&w, &[1])).down();
    let set: HashSet<Vec<u8>> = blobs(&w, &[0]).into_iter().collect();
    let sink = w.sink();
    let gate = FakeGate::new(GateOutcome::Deny);
    let mut s = Session::new(&p, &user, &sink, &*gate).with_dedicated(&ded, &set);
    assert_eq!(listed(s.handle(&[wire_list()]).await), blobs(&w, &[0]));
    assert_eq!(w.last_code().as_deref(), Some("upstream_unavailable"));
}

#[tokio::test]
async fn a_dead_private_agent_refuses_only_its_own_key() {
    let w = World::new();
    let p = policy_all(&w);
    let ded = FakeUpstream::new(blobs(&w, &[0])).down();
    let user = FakeUpstream::new(blobs(&w, &[1]));
    let set: HashSet<Vec<u8>> = blobs(&w, &[0]).into_iter().collect();
    let sink = w.sink();
    let gate = FakeGate::new(GateOutcome::Allow { decision: None });
    let mut s = Session::new(&p, &user, &sink, &*gate).with_dedicated(&ded, &set);
    assert_eq!(s.handle(&w.good_bind()).await, Answer::Success);
    let a = s.handle(&sign_body(&w.keys[0].key_blob())).await;
    assert_eq!(a, Answer::Failure);
    assert_eq!(w.last_code().as_deref(), Some("upstream_unavailable"));
    assert_eq!(forwarded_signs(&user), 0, "never retried on the user agent");
    let a = s.handle(&sign_body(&w.keys[1].key_blob())).await;
    assert!(matches!(a, Answer::SignResponse(_)), "{a:?}");
}

fn wire_list() -> u8 {
    crate::ssh_proxy::wire::SSH_AGENTC_REQUEST_IDENTITIES
}
