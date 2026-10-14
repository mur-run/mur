//! Which router keys `mur channel verify` is willing to trust.
//!
//! Deliberately stricter than `mur_channel::sign::resolve_writer_pubkey`, which
//! falls back to the current `identity.pub` whenever a `key_version` is not in
//! the chain and skips rotation lines it cannot parse. A verifier that quietly
//! substitutes the current key for an unknown version turns "I cannot tell" into
//! a verdict. Here an unknown version resolves to nothing.
//!
//! Sources, and only these: the router's current `identity.pub`, and the router's
//! `rotations.jsonl` when every line of it parses and the whole chain verifies.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::Path;

use mur_common::identity::{
    AgentIdentity, ChainOptions, RotationAttestation, decode_pubkey, verify_chain,
};

/// What the router's rotation record says.
enum Chain {
    /// No `rotations.jsonl`: an agent whose key was never rotated.
    Absent,
    /// Present but unreadable, unparseable, or failing `verify_chain`. Fails
    /// closed: no key version resolves, not even the current one.
    Invalid,
    /// Fully verified: `(key_version, pubkey)` for every entry, and the head's.
    Valid {
        by_version: Vec<(u32, [u8; 32])>,
        head: [u8; 32],
    },
}

/// Router key source for one verification run. Records every lookup that came
/// back empty, so the caller can refuse to call the result `ok`.
pub struct RouterKeys {
    chain: Chain,
    current: Option<[u8; 32]>,
    misses: RefCell<BTreeSet<Option<u32>>>,
}

impl RouterKeys {
    pub fn load(router_dir: &Path) -> Self {
        Self {
            chain: load_chain(router_dir),
            current: AgentIdentity::load_pubkey(router_dir).ok(),
            misses: RefCell::new(BTreeSet::new()),
        }
    }

    /// The key for `key_version`, or `None` (recorded as a miss).
    pub fn resolve(&self, key_version: Option<u32>) -> Option<[u8; 32]> {
        let key = match (&self.chain, key_version) {
            (Chain::Invalid, _) => None,
            (Chain::Valid { head, .. }, None) => Some(*head),
            (Chain::Valid { by_version, .. }, Some(v)) => {
                by_version.iter().find(|(kv, _)| *kv == v).map(|(_, k)| *k)
            }
            // Never rotated: version 0 is the real version of every such agent
            // (`channel_writer::head_sealer`), and `None` is a pre-versioning line.
            (Chain::Absent, None | Some(0)) => self.current,
            (Chain::Absent, Some(_)) => None,
        };
        if key.is_none() {
            self.misses.borrow_mut().insert(key_version);
        }
        key
    }

    /// Key versions that could not be resolved so far, in order.
    pub fn misses(&self) -> Vec<Option<u32>> {
        self.misses.borrow().iter().copied().collect()
    }

    /// Forget recorded misses: one `RouterKeys` is shared across channels.
    pub fn reset_misses(&self) {
        self.misses.borrow_mut().clear();
    }
}

fn load_chain(router_dir: &Path) -> Chain {
    let text = match std::fs::read_to_string(router_dir.join("rotations.jsonl")) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Chain::Absent,
        Err(_) => return Chain::Invalid,
    };
    let mut chain: Vec<RotationAttestation> = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str(line) {
            Ok(a) => chain.push(a),
            Err(_) => return Chain::Invalid, // a line we cannot read is a hole
        }
    }
    // Emergency rotations carry no signature from the old key, so they prove
    // nothing about continuity. Not trusted here.
    let Ok(outcome) = verify_chain(
        &chain,
        ChainOptions {
            allow_emergency: false,
        },
    ) else {
        return Chain::Invalid;
    };
    let Ok(head) = decode_pubkey(&outcome.head_pubkey) else {
        return Chain::Invalid;
    };
    let mut by_version = Vec::with_capacity(chain.len());
    for a in &chain {
        let Ok(k) = decode_pubkey(&a.new_pubkey) else {
            return Chain::Invalid;
        };
        by_version.push((a.new_key_version, k));
    }
    Chain::Valid { by_version, head }
}
