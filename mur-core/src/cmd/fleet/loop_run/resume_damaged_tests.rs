//! #2081 item 10: a damaged channel log must not let resume settle on looser
//! limits than the run was paused with.

use super::super::pause_events::{PauseReason, RunBounds};
use super::damaged::{checked_resume, resume_from_damaged};
use super::tests::{RUN, paused_home, signed};
use super::*;
use crate::channel_writer::{CONTROL_PLANE_AGENT, Signer, append_as_writer, plant_writer_identity};
use mur_common::channel::ChannelActor;

/// The fixture fleet's channel and name (`resume_tests::fleet`).
const CHANNEL: &str = "fleet-dev";
const FLEET: &str = "dev";
/// The variable `channel_verify::require_sig_from_env` reads.
const REQUIRE_SIG_ENV: &str = "MUR_CHANNEL_REQUIRE_SIG";
/// What `require_intact` says, and no other refusal does.
const DAMAGE_REFUSAL: &str = "cannot vouch for the original limits";

/// The bounds the fixture checkpoint carries, and the tightest pause.
fn tight() -> RunBounds {
    RunBounds {
        deadline_ms: 600_000,
        stuck_ms: Some(300_000),
        cost_usd: Some(5.0),
    }
}

fn loose() -> RunBounds {
    RunBounds {
        deadline_ms: 900_000,
        stuck_ms: None,
        cost_usd: Some(20.0),
    }
}

/// A paused fleet whose channel holds three control-plane-signed pauses of
/// the run, written by the real writer so the head is sealed: loose, tight,
/// loose. The tight one is line 2.
fn home_with_three_pauses() -> tempfile::TempDir {
    let home = paused_home(None);
    plant_writer_identity(home.path());
    let svc = mur_channel::ChannelService::open(home.path()).unwrap();
    svc.create_for_fleet(
        FLEET,
        CONTROL_PLANE_AGENT,
        &["pm".into()],
        mur_channel::service::ChannelOwner::Legacy,
    )
    .unwrap();
    for (iteration, b) in [(1, loose()), (2, tight()), (2, loose())] {
        let note = FleetRunPayload::RunPaused {
            run_id: RUN.into(),
            iteration_run_id: format!("{RUN}-{iteration}"),
            iteration,
            spent_usd: 1.0,
            active_elapsed_ms: 60_000,
            reason: PauseReason::Operator,
            bounds: Some(b),
        }
        .to_note_payload();
        append_as_writer(
            &svc,
            home.path(),
            CHANNEL,
            Signer::ControlPlane,
            ChannelActor::System,
            EventKind::Note,
            note,
            None,
        )
        .unwrap();
    }
    home
}

/// The two ways the tightest pause can go missing from a log that still
/// reads: its line no longer parses, or it is gone.
#[derive(Debug, Clone, Copy)]
enum Loss {
    Unparseable,
    Deleted,
    /// Moved to the end of the log, then cut off with everything after it.
    /// Only the signed head notices a missing tail; the chain does not.
    TailCut,
}

const EVERY_LOSS: [Loss; 3] = [Loss::Unparseable, Loss::Deleted, Loss::TailCut];

fn lose_the_tight_pause(home: &Path, how: Loss) {
    let path = mur_channel::ChannelStore::new(home)
        .events_path(CHANNEL)
        .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    assert_eq!(lines.len(), 3, "fixture: three pauses, one line each");
    match how {
        Loss::Unparseable => {
            let half = lines[1].len() / 2;
            lines[1].truncate(half);
        }
        Loss::Deleted => {
            lines.remove(1);
        }
        Loss::TailCut => lines.truncate(1),
    }
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
}

fn resume(home: &Path, cp: &Checkpoint, require_sig: bool) -> Result<(ResumeFrom, Option<String>)> {
    let fleet = store::load_fleet(home, FLEET).unwrap();
    checked_resume(home, &fleet, cp, true, require_sig)
}

fn checkpoint_of(home: &Path) -> Checkpoint {
    checkpoint::load(home, FLEET, RUN).unwrap().unwrap()
}

fn bounds_of(r: &ResumeFrom) -> (u64, Option<f64>) {
    (r.deadline.as_millis() as u64, r.cost_usd)
}

/// The control: an intact log is not damage, and the signed fold decides.
#[test]
fn an_intact_log_resumes_under_the_signed_bounds() {
    let home = home_with_three_pauses();
    // The checkpoint claims more than any pause; it must not be consulted.
    let mut cp = checkpoint_of(home.path());
    cp.cost_usd = Some(99.0);
    for require_sig in [false, true] {
        let (from, damage) = resume(home.path(), &cp, require_sig).unwrap();
        assert_eq!(damage, None, "an intact log read as damaged");
        assert!(from.bounds_authenticated, "{from:?}");
        assert_eq!(bounds_of(&from), (600_000, Some(5.0)));
    }
}

/// The bug: with the tightest pause unreadable or cut out, the fold used to
/// settle on the looser pauses ($20, 15 min) and call them authenticated.
#[test]
fn a_lost_tightest_pause_does_not_widen_the_resume() {
    for how in EVERY_LOSS {
        let home = home_with_three_pauses();
        lose_the_tight_pause(home.path(), how);

        // What the signed fold alone now yields — the looser bounds.
        let events = mur_channel::ChannelService::open(home.path())
            .unwrap()
            .load_events(CHANNEL)
            .unwrap();
        let folded = signed_pause(home.path(), CHANNEL, &events, RUN)
            .unwrap()
            .expect("a looser pause still verifies");
        assert_eq!(folded.bounds, Some(loose()), "{how:?}: fixture");

        let cp = checkpoint_of(home.path());
        let (from, damage) = resume(home.path(), &cp, false).unwrap();
        let why = damage.unwrap_or_else(|| panic!("{how:?}: the damage went unnoticed"));
        assert!(why.contains("integrity"), "{how:?}: {why}");
        assert_eq!(
            bounds_of(&from),
            (600_000, Some(5.0)),
            "{how:?}: resumed under the looser pause"
        );
        assert_eq!(
            from.stuck,
            mur_common::limits::Stuck::After(Duration::from_millis(300_000)),
            "{how:?}"
        );
        assert!(
            !from.bounds_authenticated,
            "{how:?}: nothing vouches for limits read off a damaged log"
        );
    }
}

/// On a damaged log the checkpoint may tighten the limits, never widen them:
/// an edited checkpoint cannot lift the run above the pauses still readable.
#[test]
fn an_edited_checkpoint_cannot_widen_a_damaged_log() {
    let home = home_with_three_pauses();
    lose_the_tight_pause(home.path(), Loss::Unparseable);
    let mut cp = checkpoint_of(home.path());
    cp.cost_usd = None;
    cp.deadline_ms = 9_000_000;
    cp.stuck_ms = None;
    let (from, damage) = resume(home.path(), &cp, false).unwrap();
    assert!(damage.is_some());
    assert_eq!(bounds_of(&from), (900_000, Some(20.0)));
}

/// The fold itself: counters take the highest, bounds the tightest, and the
/// result is never reported as authenticated — with or without a pause left.
#[test]
fn the_damaged_fold_takes_the_tightest_of_pauses_and_checkpoint() {
    let home = paused_home(None);
    let cp = checkpoint_of(home.path());
    let left = signed(9.0, 70_000, 4, Some(loose()));
    let from = resume_from_damaged(&cp, Some(left), true).unwrap();
    assert_eq!(bounds_of(&from), (600_000, Some(5.0)));
    assert_eq!((from.spent_usd, from.iteration), (9.0, 4));
    assert!(!from.bounds_authenticated);

    let alone = resume_from_damaged(&cp, None, true).unwrap();
    assert_eq!(bounds_of(&alone), (600_000, Some(5.0)));
    assert_eq!(
        (alone.spent_usd, alone.iteration),
        (cp.spent_usd, cp.iteration)
    );
    assert!(!alone.bounds_authenticated);
}

/// Under `MUR_CHANNEL_REQUIRE_SIG` the damage refuses the resume, although
/// two signed pauses still verify. The refusal names the damage, not "no
/// signed run_paused", and nothing is touched.
#[tokio::test]
async fn require_sig_refuses_a_damaged_log() {
    use mur_common::paths::{FLEET_RUN_LOCK_FILE, FLEET_RUN_OWNER_FILE, fleet_state_dir};

    let mut envg = mur_common::test_env::EnvGuard::hold();
    envg.set_var(REQUIRE_SIG_ENV, "1");
    assert!(crate::channel_verify::require_sig_from_env());

    for how in EVERY_LOSS {
        let home = home_with_three_pauses();
        lose_the_tight_pause(home.path(), how);

        let err = cmd_fleet_resume(home.path(), FLEET).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains(DAMAGE_REFUSAL), "{how:?}: {msg}");
        assert!(
            msg.contains("mur channel verify fleet-dev"),
            "{how:?}: {msg}"
        );

        let state = fleet_state_dir(home.path(), FLEET);
        for f in [FLEET_RUN_LOCK_FILE, FLEET_RUN_OWNER_FILE] {
            assert!(!state.join(f).exists(), "{how:?}: the run lock was taken");
        }
        assert!(control::paused_path(home.path(), FLEET).exists());
        assert!(checkpoint::load(home.path(), FLEET, RUN).unwrap().is_some());
    }
}

/// An intact log under the flag is not refused for damage (the control for
/// the test above), and a channel with no log at all keeps its own refusal.
#[test]
fn require_sig_only_refuses_for_damage_when_there_is_damage() {
    let home = home_with_three_pauses();
    let cp = checkpoint_of(home.path());
    assert!(resume(home.path(), &cp, true).is_ok());

    let bare = paused_home(None);
    let err = resume(bare.path(), &checkpoint_of(bare.path()), true).unwrap_err();
    let msg = format!("{err:#}");
    assert!(!msg.contains(DAMAGE_REFUSAL), "{msg}");
    assert!(msg.contains("no signed run_paused"), "{msg}");
}
