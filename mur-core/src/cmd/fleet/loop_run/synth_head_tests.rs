//! #2081 finding C: a fleet's own router (`router: orchestrator`) records its
//! synthesis on a channel whose every other line is the control plane's. The
//! line keeps the router's signature, the head stays the control plane's, and
//! the write succeeds whichever of the two wrote first.

use std::path::{Path, PathBuf};

use mur_channel::ChannelService;
use mur_common::channel::EventKind;
use mur_common::fleet::{Fleet, FleetLoop, FleetStep};
use mur_common::identity::AgentIdentity;

use super::*;
use crate::channel_writer::{CONTROL_PLANE_AGENT, append_with_key, plant_identity_for, writer_key};
use crate::cmd::channel_integrity::check;

const MARKER: &str = "RESEARCH_DONE";
const FLEET_ROUTER: &str = "orchestrator";
const MEMBER: &str = "member";
const CHANNEL: &str = "fleet-dev";
/// What each fake turn reports, per direction.
const TURN_TOKENS: u64 = 1000;

fn fleet(router: Option<&str>) -> Fleet {
    Fleet {
        name: "dev".into(),
        display_name: String::new(),
        goal: "research question".into(),
        router: router.map(str::to_string),
        members: vec![MEMBER.into()],
        team_id: None,
        channel_id: CHANNEL.into(),
        procedure: vec![FleetStep {
            id: "s1".into(),
            member: MEMBER.into(),
            task: String::new(),
            depends_on: vec![],
        }],
        rules: vec![],
        skills: vec![],
        loop_cfg: Some(FleetLoop {
            done_when: format!("marker:{MARKER}"),
            ..serde_yaml::from_str("{}").unwrap()
        }),
        parallel: None,
        hitl: None,
        requires_programs: vec![],
        limits: None,
        needs: vec![],
    }
}

struct Home {
    dir: tempfile::TempDir,
    control_plane: Option<AgentIdentity>,
    fleet_router: AgentIdentity,
}

impl Home {
    /// A fleet channel on a home holding the fleet router's key and, unless
    /// `keyless_control_plane`, the control plane's.
    fn build(keyless_control_plane: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let control_plane =
            (!keyless_control_plane).then(|| plant_identity_for(dir.path(), CONTROL_PLANE_AGENT));
        let fleet_router = plant_identity_for(dir.path(), FLEET_ROUTER);
        crate::cmd::fleet::store::save_fleet(dir.path(), &fleet(Some(FLEET_ROUTER))).unwrap();
        ChannelService::open(dir.path())
            .unwrap()
            .create_for_fleet(
                "dev",
                FLEET_ROUTER,
                &[MEMBER.into()],
                mur_channel::service::ChannelOwner::Legacy,
            )
            .unwrap();
        Self {
            dir,
            control_plane,
            fleet_router,
        }
    }

    fn new() -> Self {
        Self::build(false)
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// An anchored write under `signer`'s own key: what every disk-key
    /// writer did before a fleet router's line was sealed for it.
    fn anchored(
        &self,
        signer: &str,
        actor: ChannelActor,
        kind: EventKind,
        text: &str,
    ) -> anyhow::Result<ChannelEvent> {
        let key = writer_key(self.path(), signer)?.expect("the key is planted");
        append_with_key(
            &ChannelService::open(self.path())?,
            self.path(),
            CHANNEL,
            key,
            &self.path().join("agents").join(signer),
            actor,
            kind,
            serde_json::json!({ "text": text }),
            None,
        )
    }

    /// What the loop and the DAG write: a `System` line, control-plane key.
    fn system_note(&self, text: &str) -> anyhow::Result<ChannelEvent> {
        self.anchored(
            CONTROL_PLANE_AGENT,
            ChannelActor::System,
            EventKind::Note,
            text,
        )
    }

    fn synthesis(&self) -> anyhow::Result<ChannelEvent> {
        synth::record_synthesis(self.path(), &fleet(Some(FLEET_ROUTER)), "the report")
    }

    fn head(&self) -> mur_channel::head::Head {
        mur_channel::head_io::read_head(&mur_channel::anchor::heads_dir(self.path()), CHANNEL)
            .unwrap()
            .expect("a head")
    }

    /// The head covers the whole log, is the control plane's, nothing dirty.
    fn assert_control_plane_head_on_the_log(&self) {
        let head = self.head();
        let log = std::fs::read(
            self.path()
                .join("channels")
                .join(CHANNEL)
                .join("events.jsonl"),
        )
        .unwrap();
        assert_eq!(head.lines, mur_channel::head::chain_log(&log).lines);
        let cp = self.control_plane.as_ref().expect("a control-plane key");
        assert!(
            head.verify(&cp.verifying_key_bytes()),
            "the head must be the control plane's"
        );
        assert!(!head.verify(&self.fleet_router.verifying_key_bytes()));
        assert!(!mur_channel::anchor::is_dirty(self.path(), CHANNEL));
    }

    fn assert_intact(&self) {
        let r = check(self.path(), CHANNEL).unwrap();
        assert!(r.is_intact(), "channel reported damaged:\n{}", r.render());
    }

    /// The synthesis line is the fleet router's own, under its own key.
    fn assert_signed_by_the_fleet_router(&self, ev: &ChannelEvent) {
        assert_eq!(
            ev.actor,
            ChannelActor::Agent {
                id: FLEET_ROUTER.into()
            }
        );
        assert!(
            crate::channel_verify::verify_event(self.path(), CHANNEL, ev, true),
            "the synthesis must verify under the fleet router's key"
        );
    }
}

#[test]
fn a_synthesis_after_a_control_plane_write_is_recorded_and_anchored() {
    let h = Home::new();
    h.system_note("delegated").unwrap();

    let ev = h
        .synthesis()
        .expect("the synthesis is written and anchored, so it must not report failure");

    h.assert_signed_by_the_fleet_router(&ev);
    h.assert_control_plane_head_on_the_log();
    h.assert_intact();
    h.system_note("run finished").unwrap();
    h.assert_control_plane_head_on_the_log();
    h.assert_intact();
}

#[test]
fn a_control_plane_write_after_a_synthesis_still_seals() {
    let h = Home::new();
    let ev = h.synthesis().unwrap();
    h.assert_signed_by_the_fleet_router(&ev);
    h.assert_control_plane_head_on_the_log();

    h.system_note("run finished")
        .expect("the control plane must still be able to seal after a synthesis");

    h.assert_control_plane_head_on_the_log();
    h.assert_intact();
}

/// A head the fleet router sealed under its own key before this fix is taken
/// over on its next synthesis, not left stranded.
#[test]
fn a_head_the_fleet_router_sealed_earlier_is_resealed_under_the_control_plane() {
    let h = Home::new();
    h.anchored(
        FLEET_ROUTER,
        ChannelActor::Agent {
            id: FLEET_ROUTER.into(),
        },
        EventKind::Message,
        "an earlier report",
    )
    .unwrap();
    assert!(h.head().verify(&h.fleet_router.verifying_key_bytes()));
    assert!(
        h.system_note("stranded").is_err(),
        "fixture: the control plane cannot advance a head the fleet router sealed"
    );

    h.synthesis().unwrap();

    h.assert_control_plane_head_on_the_log();
    h.system_note("writable again").unwrap();
    h.assert_control_plane_head_on_the_log();
    h.assert_intact();
}

/// The default router is the control plane: one key signs and seals, as ever.
#[test]
fn the_default_routers_synthesis_is_signed_and_sealed_by_one_key() {
    let h = Home::new();
    h.system_note("delegated").unwrap();

    let ev = synth::record_synthesis(h.path(), &fleet(None), "the report").unwrap();

    assert_eq!(
        ev.actor,
        ChannelActor::Agent {
            id: CONTROL_PLANE_AGENT.into()
        }
    );
    assert!(crate::channel_verify::verify_event(
        h.path(),
        CHANNEL,
        &ev,
        true
    ));
    h.assert_control_plane_head_on_the_log();
    h.assert_intact();
}

/// A home with no control-plane key has no other sealer to collide with: the
/// fleet router seals its own head there, as it always has.
#[test]
fn without_a_control_plane_key_the_fleet_router_seals_its_own_head() {
    let h = Home::build(true);

    let ev = h.synthesis().unwrap();

    h.assert_signed_by_the_fleet_router(&ev);
    let head = h.head();
    assert_eq!(head.seq, ev.seq);
    assert!(head.verify(&h.fleet_router.verifying_key_bytes()));
    assert!(!mur_channel::anchor::is_dirty(h.path(), CHANNEL));
}

/// The control-plane key is there but unreadable: the synthesis still lands,
/// past a head that is left alone, and the control plane's next write covers
/// it. It is never sealed under the fleet router's key.
#[cfg(unix)]
#[test]
fn an_unreadable_control_plane_key_neither_blocks_nor_reseals_a_synthesis() {
    use std::os::unix::fs::PermissionsExt;

    let h = Home::new();
    h.system_note("delegated").unwrap();
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

    let written = h.synthesis();
    lock(0o600).unwrap();
    let ev = written.expect("an unreadable control-plane key must not lose the synthesis");

    h.assert_signed_by_the_fleet_router(&ev);
    assert!(h.head().seq < ev.seq, "the head is left where it was");
    assert!(!mur_channel::anchor::is_dirty(h.path(), CHANNEL));
    h.assert_intact();
    h.system_note("covers the synthesis").unwrap();
    h.assert_control_plane_head_on_the_log();
    h.assert_intact();
}

// ── a whole loop run, with fake agents on unix sockets ─────────────────────

#[cfg(unix)]
fn running_lock(home: &Path, name: &str, sock: &Path) {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("running.lock"),
        serde_json::json!({
            "schema": 1, "uuid": "u", "name": name, "pid": 1, "ppid": 1,
            "started_at": "t", "binary_version": "test", "proto_version": 1,
            "transports": { "stdio": true, "unix_socket": sock.to_str().unwrap() },
            "card_digest": "d", "capabilities": [],
        })
        .to_string(),
    )
    .unwrap();
}

/// A fake agent: every request is answered with a completed task carrying
/// `text` and `TURN_TOKENS` each way. `on_delegate` runs for a delegation.
#[cfg(unix)]
fn serve(sock: PathBuf, text: &'static str, on_delegate: impl Fn() + Send + 'static) {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(conn) = conn else { break };
            let mut w = conn.try_clone().unwrap();
            let mut line = String::new();
            if BufReader::new(conn).read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            if req["method"] == "channel/delegate" {
                on_delegate();
            }
            let task = serde_json::json!({
                "id": req["params"]["task_id"],
                "status": { "state": "completed" },
                "usage": { "input_tokens": TURN_TOKENS, "output_tokens": TURN_TOKENS },
                "messages": [{ "role": "agent", "parts": [{ "kind": "text", "text": text }] }],
            });
            let reply = serde_json::json!({"jsonrpc":"2.0","id":req["id"],"result":task});
            let _ = writeln!(w, "{reply}");
        }
    });
}

/// One marker-policy run of a `router: orchestrator` fleet: the member works,
/// the fleet router synthesises, the run converges. Before the fix the
/// synthesis line was written but its seal failed, the error was swallowed as
/// "0 tokens", and the run was billed for the member's turn alone.
#[cfg(unix)]
#[tokio::test]
async fn a_custom_router_run_bills_its_synthesis_and_ends_intact() {
    if std::env::var("MUR_TEST_SOCKETS").as_deref() != Ok("1") {
        eprintln!("skipping a_custom_router_run_bills_its_synthesis: set MUR_TEST_SOCKETS=1");
        return;
    }
    let h = Home::new();
    let home = h.path().to_path_buf();
    let member = plant_identity_for(&home, MEMBER);
    let member_home = home.clone();
    serve(home.join("m.sock"), "findings", move || {
        // A sealed member signs with its own key and never moves the head.
        ChannelService::open(&member_home)
            .unwrap()
            .append_signed(
                CHANNEL,
                &member,
                0,
                ChannelActor::Agent { id: MEMBER.into() },
                EventKind::Message,
                serde_json::json!({ "text": "findings" }),
                None,
            )
            .unwrap();
    });
    running_lock(&home, MEMBER, &home.join("m.sock"));
    serve(home.join("r.sock"), "the report", || {});
    running_lock(&home, FLEET_ROUTER, &home.join("r.sock"));

    let (price_per_1k, _) = fleet_price_per_1k(&home);
    let (stop, iterations, spent) = run_guarded(&home, "dev", None, None, None, None, None, None)
        .await
        .unwrap();

    assert_eq!((stop, iterations), (LoopStop::Converged, 1));
    // Two turns, the member's and the synthesis, each `TURN_TOKENS` both ways.
    let both_turns = iteration_cost_usd(4 * TURN_TOKENS, price_per_1k);
    assert!(
        (spent - both_turns).abs() < 1e-9,
        "the synthesis turn must be billed: spent {spent}, expected {both_turns}"
    );
    let events = ChannelService::open(&home)
        .unwrap()
        .load_events(CHANNEL)
        .unwrap();
    let synthesis = events
        .iter()
        .find(|e| {
            e.actor
                == ChannelActor::Agent {
                    id: FLEET_ROUTER.into(),
                }
        })
        .expect("the synthesis is on the channel");
    h.assert_signed_by_the_fleet_router(synthesis);
    h.assert_control_plane_head_on_the_log();
    h.assert_intact();
}

/// #2520: the router's turn is paid for once it returns, whether or not its
/// synthesis can then be written. A write that fails must not hide the usage.
#[cfg(unix)]
#[test]
fn a_synthesis_that_cannot_be_written_still_reports_its_tokens() {
    use std::os::unix::fs::PermissionsExt;

    if std::env::var("MUR_TEST_SOCKETS").as_deref() != Ok("1") {
        eprintln!("skipping a_synthesis_that_cannot_be_written: set MUR_TEST_SOCKETS=1");
        return;
    }
    let h = Home::new();
    h.system_note("delegated").unwrap();
    serve(h.path().join("r.sock"), "the report", || {});
    running_lock(h.path(), FLEET_ROUTER, &h.path().join("r.sock"));
    let channel_dir = h.path().join("channels").join(CHANNEL);
    let log = channel_dir.join("events.jsonl");
    let lock = |dir_mode, log_mode| {
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(log_mode)).unwrap();
        std::fs::set_permissions(&channel_dir, std::fs::Permissions::from_mode(dir_mode)).unwrap();
    };
    lock(0o555, 0o444);
    if std::fs::OpenOptions::new().append(true).open(&log).is_ok() {
        // Running as root: the mode does not deny anything, nothing to test.
        lock(0o755, 0o644);
        return;
    }
    let fixture = h.synthesis();

    let tokens = synth::synthesize_via_router(
        h.path(),
        &fleet(Some(FLEET_ROUTER)),
        "goal",
        MARKER,
        "findings",
    );
    lock(0o755, 0o644);

    assert!(fixture.is_err(), "fixture: the synthesis write must fail");
    assert_eq!(
        tokens
            .expect("the turn ran, so its usage is reported")
            .total(),
        2 * TURN_TOKENS
    );
}

/// A loop iteration is priced turn by turn at each answering model's own
/// rate and billing, as a one-shot run is. Before, every token of the
/// iteration — the subscription member's included — was billed at the
/// registry's dearest metered rate.
#[cfg(unix)]
#[tokio::test]
async fn a_loop_iteration_bills_only_metered_turns_at_their_own_rates() {
    if std::env::var("MUR_TEST_SOCKETS").as_deref() != Ok("1") {
        eprintln!("skipping a_loop_iteration_bills_only_metered_turns: set MUR_TEST_SOCKETS=1");
        return;
    }
    let h = Home::new();
    let home = h.path().to_path_buf();
    std::fs::write(
        home.join("models.yaml"),
        "schema_version: 1\nmodels:\n  \
         sub:\n    provider: claude\n    model: opus\n    billing: subscription\n    \
         input_cost_per_1k: 0.005\n    output_cost_per_1k: 0.025\n  \
         api:\n    provider: openai\n    model: gpt-x\n    billing: usage_billed\n    \
         input_cost_per_1k: 0.001\n    output_cost_per_1k: 0.002\n",
    )
    .unwrap();
    super::cost_tests::plant_member(&home, MEMBER, "sub");
    super::cost_tests::plant_member(&home, FLEET_ROUTER, "api");
    let member = plant_identity_for(&home, MEMBER);
    let member_home = home.clone();
    serve(home.join("m.sock"), "findings", move || {
        ChannelService::open(&member_home)
            .unwrap()
            .append_signed(
                CHANNEL,
                &member,
                0,
                ChannelActor::Agent { id: MEMBER.into() },
                EventKind::Message,
                serde_json::json!({ "text": "findings" }),
                None,
            )
            .unwrap();
    });
    running_lock(&home, MEMBER, &home.join("m.sock"));
    serve(home.join("r.sock"), "the report", || {});
    running_lock(&home, FLEET_ROUTER, &home.join("r.sock"));

    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.unset_var("MUR_FLEET_COST_PER_1K");
    let (stop, _, spent) = run_guarded(&home, "dev", None, None, None, None, None, None)
        .await
        .unwrap();

    assert_eq!(stop, LoopStop::Converged);
    // The member's subscription turn is free; the router's synthesis is
    // `TURN_TOKENS` in at $0.001/1k and `TURN_TOKENS` out at $0.002/1k.
    let synthesis = (TURN_TOKENS as f64 / 1000.0) * (0.001 + 0.002);
    assert!(
        (spent - synthesis).abs() < 1e-9,
        "only the metered router turn is billed: spent {spent}, expected {synthesis}"
    );
}
