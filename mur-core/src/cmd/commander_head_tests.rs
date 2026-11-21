//! A commander directive on a channel the router also writes: the directive
//! keeps its commander signature, the head stays the router's, and the
//! integrity check reads the result as intact, whichever of the two wrote
//! first.

use std::path::Path;

use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

use super::*;
use crate::channel_writer::{
    CONTROL_PLANE_AGENT, append_with_key, plant_writer_identity, writer_key,
};
use crate::cmd::channel_integrity::check;

const FLEET: &str = "dev";
const CHANNEL: &str = "fleet-dev";

struct Home {
    dir: TempDir,
    router: AgentIdentity,
    commander: AgentIdentity,
}

impl Home {
    /// A fleet channel on a home with a router key and a pinned commander.
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let router = plant_writer_identity(dir.path());
        let commander = AgentIdentity::generate();
        commander.save(&dir.path().join(COMMANDER_DIR)).unwrap();
        ChannelService::open(dir.path())
            .unwrap()
            .create_for_fleet(
                FLEET,
                CONTROL_PLANE_AGENT,
                &["pm".into()],
                mur_channel::service::ChannelOwner::Legacy,
            )
            .unwrap();
        Self {
            dir,
            router,
            commander,
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// What the control plane does for every `System` line it writes.
    fn router_note(&self, text: &str) -> anyhow::Result<ChannelEvent> {
        let svc = ChannelService::open(self.path())?;
        let key = writer_key(self.path(), CONTROL_PLANE_AGENT)?.expect("the router key is planted");
        append_with_key(
            &svc,
            self.path(),
            CHANNEL,
            key,
            &self.path().join("agents").join(CONTROL_PLANE_AGENT),
            ChannelActor::System,
            EventKind::Note,
            serde_json::json!({ "text": text }),
            None,
        )
    }

    fn directive(&self, kind: &str, budget: Option<f64>) -> anyhow::Result<()> {
        cmd_commander_directive(self.path(), FLEET, kind, budget, 1000)
    }

    /// The head covers the whole log, is the router's, and nothing is dirty.
    fn assert_router_head_on_the_log(&self) {
        let head =
            mur_channel::head_io::read_head(&mur_channel::anchor::heads_dir(self.path()), CHANNEL)
                .unwrap()
                .expect("a head");
        let log = std::fs::read(
            self.path()
                .join("channels")
                .join(CHANNEL)
                .join("events.jsonl"),
        )
        .unwrap();
        assert_eq!(head.lines, mur_channel::head::chain_log(&log).lines);
        assert!(
            head.verify(&self.router.verifying_key_bytes()),
            "the head must be the router's"
        );
        assert!(!head.verify(&self.commander.verifying_key_bytes()));
        assert!(!mur_channel::anchor::is_dirty(self.path(), CHANNEL));
    }

    fn assert_intact(&self) {
        let r = check(self.path(), CHANNEL).unwrap();
        assert!(r.is_intact(), "channel reported damaged:\n{}", r.render());
    }
}

#[test]
fn a_directive_after_a_router_write_succeeds_and_leaves_the_channel_intact() {
    let h = Home::new();
    h.router_note("run started").unwrap();

    h.directive("budget_ceiling", Some(1.0))
        .expect("the directive is written and anchored, so it must not report failure");

    assert_eq!(
        governance_state(h.path(), FLEET).unwrap().budget_ceiling,
        Some(1.0)
    );
    h.assert_router_head_on_the_log();
    h.assert_intact();
    h.router_note("still writable").unwrap();
    h.assert_router_head_on_the_log();
    h.assert_intact();
}

#[test]
fn a_router_write_after_a_directive_succeeds_and_leaves_the_channel_intact() {
    let h = Home::new();
    h.directive("kill", None).unwrap();
    h.assert_router_head_on_the_log();
    h.assert_intact();

    h.router_note("run refused: killed")
        .expect("the router must still be able to seal after a directive");

    assert!(governance_state(h.path(), FLEET).unwrap().killed);
    h.assert_router_head_on_the_log();
    h.assert_intact();
}

/// A head the commander sealed under its own key before this fix is taken
/// over by the router on the next directive, not left stranded.
#[test]
fn a_head_the_commander_sealed_earlier_is_resealed_under_the_router() {
    let h = Home::new();
    ChannelService::open(h.path())
        .unwrap()
        .append_and_anchor_v1(
            CHANNEL,
            &h.commander,
            0,
            &h.path().join(COMMANDER_DIR),
            None,
            ChannelActor::System,
            EventKind::Note,
            serde_json::json!({ COMMANDER_DIRECTIVE_KEY: {
                "kind": "kill", "fleet": FLEET, "budget_usd": null,
                "nonce": "old", "issued_at_ms": 1,
            }}),
            Some("old".into()),
        )
        .unwrap();
    assert!(
        h.router_note("stranded").is_err(),
        "fixture: the router cannot advance a commander-sealed head"
    );

    h.directive("resume", None).unwrap();

    h.assert_router_head_on_the_log();
    h.router_note("writable again").unwrap();
    h.assert_router_head_on_the_log();
    h.assert_intact();
}

/// Only a pinned commander key vouches for a directive line: the same payload
/// under any other key is still a bad signature.
#[test]
fn a_directive_signed_by_an_unpinned_key_is_still_reported() {
    let h = Home::new();
    h.router_note("run started").unwrap();
    let stranger = AgentIdentity::generate();
    let forged = ChannelService::open(h.path())
        .unwrap()
        .append_signed_v1(
            CHANNEL,
            &stranger,
            0,
            ChannelActor::System,
            EventKind::Note,
            serde_json::json!({ COMMANDER_DIRECTIVE_KEY: {
                "kind": "resume", "fleet": FLEET, "budget_usd": null,
                "nonce": "forged", "issued_at_ms": 2,
            }}),
            Some("forged".into()),
        )
        .unwrap();

    let r = check(h.path(), CHANNEL).unwrap();
    assert_eq!(r.unverified_seqs, vec![forged.seq], "{}", r.render());
}

/// The router key is there but unreadable: the directive must still land and
/// take effect. It goes in past the router's head, which is left alone, and
/// the router's next write covers it.
#[cfg(unix)]
#[test]
fn an_unreadable_router_key_does_not_block_or_reseal_a_directive() {
    use std::os::unix::fs::PermissionsExt;

    let h = Home::new();
    h.router_note("run started").unwrap();
    let key =
        mur_common::identity::private_key_dir(&h.path().join("agents").join(CONTROL_PLANE_AGENT))
            .join("identity.key");
    let lock = |mode| std::fs::set_permissions(&key, std::fs::Permissions::from_mode(mode));
    lock(0o000).unwrap();
    if std::fs::read(&key).is_ok() {
        // Running as root: the mode does not deny anything, nothing to test.
        lock(0o600).unwrap();
        return;
    }

    let issued = h.directive("kill", None);
    lock(0o600).unwrap();
    issued.expect("an unreadable router key must not refuse a kill");

    assert!(governance_state(h.path(), FLEET).unwrap().killed);
    assert!(!mur_channel::anchor::is_dirty(h.path(), CHANNEL));
    h.assert_intact();
    h.router_note("covers the directive").unwrap();
    h.assert_router_head_on_the_log();
    h.assert_intact();
}
