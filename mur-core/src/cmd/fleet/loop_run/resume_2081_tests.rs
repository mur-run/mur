//! #2081: resume hardening — every verified pause is folded, a channel read
//! failure is an error, and REQUIRE_SIG refuses an unsigned resume.

use super::super::pause_events::RunBounds;
use super::tests::{RUN, paused_home, signed};
use super::*;
use mur_common::channel::ChannelActor;

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

/// Item 3: an older, looser pause must not widen a newer, tighter one.
#[test]
fn every_verified_pause_is_folded_to_the_tightest_bounds() {
    let b = |d, s, c| {
        Some(RunBounds {
            deadline_ms: d,
            stuck_ms: s,
            cost_usd: c,
        })
    };
    let got = fold_pauses([
        signed(1.0, 10, 1, b(900, None, None)),
        signed(2.0, 5, 3, b(600, Some(300), Some(5.0))),
        signed(0.5, 20, 2, b(700, Some(100), Some(9.0))),
        signed(0.1, 1, 0, None),
    ])
    .unwrap();
    assert_eq!(
        (got.spent_usd, got.active_elapsed_ms, got.iteration),
        (2.0, 20, 3)
    );
    assert_eq!(got.bounds, b(600, Some(100), Some(5.0)));
    assert!(fold_pauses([]).is_none());
    assert_eq!(fold_pauses([signed(0.0, 0, 0, None)]).unwrap().bounds, None);
}

/// Item 3 end to end: a router-signed tight pause, then an older loose one
/// re-appended (validly router-signed, so it verifies). Reading the channel
/// back still yields the tight bounds.
#[test]
fn a_reappended_looser_pause_cannot_widen_the_signed_bounds() {
    let tmp = tempfile::tempdir().unwrap();
    let router = crate::channel_writer::plant_writer_identity(tmp.path());
    let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
    let ch = svc
        .create_for_workflow("dev", mur_channel::service::ChannelOwner::Legacy)
        .unwrap()
        .id;
    for (iteration, b) in [(4, tight()), (1, loose())] {
        let note = FleetRunPayload::RunPaused {
            run_id: RUN.into(),
            iteration_run_id: format!("{RUN}-{iteration}"),
            iteration,
            spent_usd: 1.0,
            active_elapsed_ms: 60_000,
            reason: super::super::pause_events::PauseReason::Operator,
            bounds: Some(b),
        }
        .to_note_payload();
        svc.append_signed(
            &ch,
            &router,
            0,
            ChannelActor::System,
            EventKind::Note,
            note,
            None,
        )
        .unwrap();
    }

    let events = svc.load_events(&ch).unwrap();
    let got = signed_pause(tmp.path(), &ch, &events, RUN)
        .unwrap()
        .expect("both pauses are router-signed");
    assert_eq!(
        got.bounds,
        Some(tight()),
        "the looser pause widened the run"
    );
    assert_eq!(got.iteration, 4, "counters never move down");
}

/// Item 2: an unreadable channel refuses the resume instead of reading as
/// "no signed pause" and falling back to the checkpoint. Nothing is touched.
#[tokio::test]
async fn an_unreadable_channel_refuses_the_resume() {
    let home = paused_home(None);
    // A directory where the log should be: present, but not readable as one.
    let log = mur_channel::ChannelStore::new(home.path())
        .events_path("fleet-dev")
        .unwrap();
    std::fs::create_dir_all(&log).unwrap();

    let err = cmd_fleet_resume(home.path(), "dev").await.unwrap_err();
    assert!(
        format!("{err:#}").contains("to verify the paused run's limits"),
        "{err:#}"
    );
    assert!(
        signed_pause(home.path(), "fleet-dev", &[], RUN).is_err(),
        "signed_pause must refuse an unreadable channel on its own"
    );
    assert!(control::paused_path(home.path(), "dev").exists());
    assert!(checkpoint::load(home.path(), "dev", RUN).unwrap().is_some());
}

/// Item 1: REQUIRE_SIG turns the unsigned-checkpoint warning into a refusal.
#[test]
fn require_sig_refuses_an_unauthenticated_resume() {
    assert!(require_authenticated("dev", false, true).is_err());
    assert!(require_authenticated("dev", false, false).is_ok());
    assert!(require_authenticated("dev", true, true).is_ok());
}
