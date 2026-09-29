//! Chip key state machine (spec §5). No TTY: keys go through the real
//! `handle_event`, and the restart runner is injected so no agent restarts.

use super::*;
use crossterm::event::{KeyEvent, KeyEventState};
use mur_common::proposal::{Proposal, ProposalKind};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

static RESTARTS: AtomicUsize = AtomicUsize::new(0);

fn fake_restart(_agent: &str) -> RestartOutcome {
    RESTARTS.fetch_add(1, AtomicOrdering::SeqCst);
    Ok(crate::cmd::agent::QuietRestart {
        ok: true,
        detail: "agent 'a' restarted (aaaa → bbbb)".into(),
        notes: vec![(false, "respawning".into())],
    })
}

fn press(c: KeyCode) -> KeyEvent {
    KeyEvent {
        code: c,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn shell_chip() -> Proposal {
    Proposal {
        label: "check".into(),
        kind: ProposalKind::Shell("git status".into()),
    }
}

fn restart_chip() -> Proposal {
    Proposal::restart("apply")
}

fn app_with(p: Proposal) -> App {
    let mut a = App::test_fixture();
    a.proposal = Some(p);
    a
}

/// Principle C regression: Enter on an empty composer must never send or
/// insert an insert-only proposal.
#[tokio::test]
async fn enter_on_insert_only_chip_sends_and_inserts_nothing() {
    let (tx, mut rx) = mpsc::channel(8);
    let mut a = app_with(shell_chip());
    let before = a.messages.len();
    handle_event(&mut a, Event::Key(press(KeyCode::Enter)), &tx).await;
    assert!(a.input_text().is_empty(), "nothing inserted");
    assert_eq!(a.messages.len(), before, "nothing sent");
    assert!(!a.streaming);
    assert!(rx.try_recv().is_err());
    assert!(a.proposal.is_some(), "chip stays");
}

#[tokio::test]
async fn tab_on_empty_composer_inserts_with_prefix_and_clears_chip() {
    let (tx, _rx) = mpsc::channel(8);
    let mut a = app_with(shell_chip());
    handle_event(&mut a, Event::Key(press(KeyCode::Tab)), &tx).await;
    assert_eq!(a.input_text(), "!git status");
    assert!(a.proposal.is_none());
}

#[tokio::test]
async fn tab_with_a_draft_leaves_draft_and_chip_alone() {
    let (tx, _rx) = mpsc::channel(8);
    let mut a = app_with(shell_chip());
    a.set_input("my draft");
    handle_event(&mut a, Event::Key(press(KeyCode::Tab)), &tx).await;
    assert!(a.input_text().starts_with("my draft"), "{}", a.input_text());
    assert!(a.proposal.is_some());
}

#[tokio::test]
async fn enter_with_a_draft_sends_the_draft_and_keeps_the_chip() {
    let (tx, _rx) = mpsc::channel(8);
    let mut a = app_with(restart_chip());
    let before = RESTARTS.load(AtomicOrdering::SeqCst);
    a.set_input("hello");
    let k = press(KeyCode::Enter);
    assert!(!handle_key_with(&mut a, &k, &tx, fake_restart));
    assert!(a.proposal.is_some());
    assert_eq!(RESTARTS.load(AtomicOrdering::SeqCst), before);
}

#[tokio::test]
async fn enter_on_idle_executable_chip_runs_restart_and_locks_submission() {
    let (tx, mut rx) = mpsc::channel(8);
    let mut a = app_with(restart_chip());
    a.context_task_id = Some("ctx-1".into());
    let k = press(KeyCode::Enter);
    assert!(handle_key_with(&mut a, &k, &tx, fake_restart));
    assert!(a.restart_in_flight, "submission locked while restarting");
    assert!(a.proposal.is_none());

    // Submission is refused while locked; the draft survives.
    a.set_input("queued");
    submit(&mut a, &tx).await;
    assert!(!a.streaming);
    assert_eq!(a.input_text(), "queued");

    let msg = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .expect("restart result")
        .expect("channel open");
    let StreamMsg::RestartDone(outcome) = msg else {
        panic!("expected RestartDone");
    };
    finish_restart(&mut a, outcome);
    assert!(!a.restart_in_flight, "unlocked after");
    assert_eq!(
        a.context_task_id.as_deref(),
        Some("ctx-1"),
        "conversation kept"
    );
    assert!(
        a.messages
            .iter()
            .any(|m| m.text.starts_with("✓ agent 'a' restarted"))
    );
}

#[tokio::test]
async fn enter_on_executable_chip_while_streaming_is_inert() {
    let (tx, _rx) = mpsc::channel(8);
    let mut a = app_with(restart_chip());
    a.streaming = true;
    let k = press(KeyCode::Enter);
    assert!(handle_key_with(&mut a, &k, &tx, fake_restart));
    assert!(!a.restart_in_flight);
    assert!(a.proposal.is_some(), "chip untouched");
    let (_, hint) = chip_line(&a).unwrap();
    assert!(hint.contains("after the turn ends"), "{hint}");
}

#[tokio::test]
async fn esc_while_streaming_keeps_the_double_esc_machine() {
    let (tx, _rx) = mpsc::channel(8);
    let mut a = app_with(shell_chip());
    a.streaming = true;
    handle_event(&mut a, Event::Key(press(KeyCode::Esc)), &tx).await;
    assert!(a.proposal.is_some(), "chip untouched");
    assert!(a.esc_hint, "first Esc still arms cancel");
}

#[tokio::test]
async fn esc_when_idle_and_empty_dismisses_the_chip() {
    let (tx, _rx) = mpsc::channel(8);
    let mut a = app_with(shell_chip());
    handle_event(&mut a, Event::Key(press(KeyCode::Esc)), &tx).await;
    assert!(a.proposal.is_none());
}

#[tokio::test]
async fn completion_overlay_wins_over_the_chip() {
    let (tx, _rx) = mpsc::channel(8);
    let mut a = app_with(shell_chip());
    a.completion = Some(complete::CompletionState {
        items: vec![],
        selected: 0,
        spaced: false,
        current: None,
    });
    handle_event(&mut a, Event::Key(press(KeyCode::Esc)), &tx).await;
    assert!(a.completion.is_none(), "overlay took Esc");
    assert!(a.proposal.is_some(), "chip unaffected");
}

#[tokio::test]
async fn hitl_esc_denies_and_leaves_the_chip() {
    let (tx, _rx) = mpsc::channel(8);
    let mut a = app_with(restart_chip());
    a.hitl = Some(stream::HitlRequest {
        hitl_id: "h1".into(),
        step_id: None,
        tool_name: "write_file".into(),
        tool_input: serde_json::json!({}),
        prompt: "approve?".into(),
        created_at: std::time::Instant::now(),
    });
    handle_event(&mut a, Event::Key(press(KeyCode::Esc)), &tx).await;
    assert!(a.hitl.is_none(), "approval decided");
    assert!(a.proposal.is_some(), "chip unaffected");
}

#[test]
fn chip_line_always_shows_the_command() {
    let a = app_with(shell_chip());
    let (what, hint) = chip_line(&a).unwrap();
    assert!(what.contains("!git status"), "{what}");
    assert!(hint.contains("Tab"), "{hint}");
    let a = app_with(restart_chip());
    let (what, _) = chip_line(&a).unwrap();
    assert!(what.contains("restart a"), "{what}");
}

// ── PR 3: the ghost is an insert-only chip (`ProposalKind::Reply`) ──

fn app_with_ghost(text: &str) -> App {
    let mut a = App::test_fixture();
    a.pending_suggestions = vec![suggest::Suggestion {
        text: text.into(),
        desc: None,
    }];
    a.reveal_suggestions();
    a
}

#[tokio::test]
async fn ghost_tab_fills_an_empty_composer() {
    let (tx, _rx) = mpsc::channel(8);
    let mut a = app_with_ghost("yes please");
    assert!(a.has_suggestion_ghost());
    handle_event(&mut a, Event::Key(press(KeyCode::Tab)), &tx).await;
    assert_eq!(a.input_text(), "yes please");
    assert!(a.proposal.is_none());
}

#[tokio::test]
async fn ghost_tab_never_touches_a_draft() {
    let (tx, _rx) = mpsc::channel(8);
    let mut a = app_with_ghost("yes please");
    a.set_input("my draft");
    handle_event(&mut a, Event::Key(press(KeyCode::Tab)), &tx).await;
    assert!(a.input_text().starts_with("my draft"), "{}", a.input_text());
    assert!(!a.input_text().contains("yes please"));
}

#[tokio::test]
async fn ghost_ignores_esc_and_enter_like_before() {
    let (tx, _rx) = mpsc::channel(8);
    let mut a = app_with_ghost("yes please");
    let esc = press(KeyCode::Esc);
    assert!(!handle_key_with(&mut a, &esc, &tx, fake_restart));
    let enter = press(KeyCode::Enter);
    assert!(!handle_key_with(&mut a, &enter, &tx, fake_restart));
    assert!(a.has_suggestion_ghost());
    assert!(a.input_text().is_empty(), "Enter never inserts the ghost");
}

#[test]
fn ghost_has_no_chip_row() {
    let a = app_with_ghost("yes please");
    assert!(chip_line(&a).is_none(), "ghost renders as placeholder only");
}

#[test]
fn agent_chip_outranks_a_ghost_on_reveal() {
    let mut a = app_with(shell_chip());
    a.pending_suggestions = vec![suggest::Suggestion {
        text: "yes".into(),
        desc: None,
    }];
    a.reveal_suggestions();
    assert_eq!(a.proposal, Some(shell_chip()));
}

#[test]
fn a_new_chip_replaces_the_ghost() {
    let mut a = app_with_ghost("yes please");
    offer(&mut a, shell_chip());
    assert_eq!(a.proposal, Some(shell_chip()));
    assert!(!a.has_suggestion_ghost());
}

#[test]
fn submit_clear_leaves_a_real_chip() {
    let mut a = app_with(shell_chip());
    a.clear_suggestion_ghost();
    assert_eq!(a.proposal, Some(shell_chip()));
}

// ── Chip vs. the reply chooser, and the auto-dismiss countdown ──

fn chooser(a: &mut App) {
    a.pending_suggestions = vec![
        suggest::Suggestion {
            text: "yes".into(),
            desc: None,
        },
        suggest::Suggestion {
            text: "no".into(),
            desc: None,
        },
    ];
    a.reveal_suggestions();
    assert!(a.completion.as_ref().is_some_and(|c| c.spaced));
}

/// Regression: with a `suggest_replies` chooser up, Tab inserted the
/// chooser's highlighted reply instead of the chip the row advertised.
#[tokio::test]
async fn tab_inserts_the_chip_even_with_the_reply_chooser_open() {
    let (tx, _rx) = mpsc::channel(8);
    let mut a = App::test_fixture();
    offer(&mut a, shell_chip());
    chooser(&mut a);
    handle_event(&mut a, Event::Key(press(KeyCode::Tab)), &tx).await;
    assert_eq!(a.input_text(), "!git status");
    assert!(a.proposal.is_none());
}

#[tokio::test]
async fn chooser_keeps_enter_and_digits_when_a_chip_is_up() {
    let (tx, _rx) = mpsc::channel(8);
    let mut a = App::test_fixture();
    offer(&mut a, restart_chip());
    chooser(&mut a);
    // Executable chip has nothing to insert: Tab stays the chooser's.
    handle_event(&mut a, Event::Key(press(KeyCode::Tab)), &tx).await;
    assert_eq!(a.input_text(), "yes");
    assert!(a.proposal.is_some(), "restart chip untouched");
}

#[test]
fn chip_countdown_arms_only_when_idle_and_expires() {
    let t0 = std::time::Instant::now();
    let mut a = app_with(shell_chip());
    a.streaming = true;
    assert!(!tick(&mut a, t0));
    assert!(a.proposal_deadline.is_none(), "no countdown mid-turn");
    a.streaming = false;
    assert!(!tick(&mut a, t0));
    assert_eq!(a.proposal_deadline, Some(t0 + PROPOSAL_TTL));
    assert!(!tick(
        &mut a,
        t0 + PROPOSAL_TTL - std::time::Duration::from_millis(1)
    ));
    assert!(a.proposal.is_some());
    assert!(tick(&mut a, t0 + PROPOSAL_TTL));
    assert!(a.proposal.is_none(), "chip dismissed at zero");
    assert!(a.proposal_deadline.is_none());
}

#[test]
fn ghost_never_counts_down() {
    let t0 = std::time::Instant::now();
    let mut a = app_with_ghost("yes please");
    assert!(!tick(&mut a, t0 + PROPOSAL_TTL * 10));
    assert!(a.has_suggestion_ghost());
    assert!(next_wake(&a, t0).is_none());
}

#[test]
fn a_new_chip_restarts_the_countdown() {
    let t0 = std::time::Instant::now();
    let mut a = app_with(shell_chip());
    tick(&mut a, t0);
    offer(&mut a, restart_chip());
    assert!(a.proposal_deadline.is_none());
    tick(&mut a, t0 + PROPOSAL_TTL);
    assert_eq!(a.proposal_deadline, Some(t0 + PROPOSAL_TTL * 2));
    assert!(a.proposal.is_some());
}

#[test]
fn countdown_shows_in_the_hint_and_wakes_each_second() {
    let now = std::time::Instant::now();
    let mut a = app_with(shell_chip());
    tick(&mut a, now);
    let (_, hint) = chip_line(&a).unwrap();
    assert!(hint.ends_with("s"), "{hint}");
    assert!(hint.contains(" · "), "{hint}");
    let wake = next_wake(&a, now).unwrap();
    assert!(wake > now && wake <= now + std::time::Duration::from_secs(1));
}

fn ctrl_k() -> KeyEvent {
    KeyEvent {
        code: KeyCode::Char('k'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

#[tokio::test]
async fn ctrl_k_pins_the_chip_and_stops_the_countdown() {
    let (tx, _rx) = mpsc::channel(4);
    let t0 = std::time::Instant::now();
    let mut a = app_with(shell_chip());
    tick(&mut a, t0);
    assert!(a.proposal_deadline.is_some());
    assert!(handle_key_with(&mut a, &ctrl_k(), &tx, fake_restart));
    assert!(a.proposal_pinned);
    assert!(
        !tick(&mut a, t0 + PROPOSAL_TTL * 10),
        "pinned never expires"
    );
    assert!(a.proposal.is_some());
    assert!(next_wake(&a, t0).is_none(), "no wake-ups while pinned");
    let (_, hint) = chip_line(&a).unwrap();
    assert!(
        hint.ends_with("pinned") && hint.contains("Ctrl+K to unpin"),
        "{hint}"
    );
}

#[tokio::test]
async fn ctrl_k_again_unpins_with_a_fresh_countdown() {
    let (tx, _rx) = mpsc::channel(4);
    let t0 = std::time::Instant::now();
    let mut a = app_with(shell_chip());
    handle_key_with(&mut a, &ctrl_k(), &tx, fake_restart);
    handle_key_with(&mut a, &ctrl_k(), &tx, fake_restart);
    assert!(!a.proposal_pinned);
    let later = t0 + PROPOSAL_TTL * 3;
    tick(&mut a, later);
    assert_eq!(a.proposal_deadline, Some(later + PROPOSAL_TTL));
}

#[tokio::test]
async fn ctrl_k_is_not_the_chips_with_a_draft_or_on_a_ghost() {
    let (tx, _rx) = mpsc::channel(4);
    let mut a = app_with(shell_chip());
    a.set_input("draft");
    assert!(!handle_key_with(&mut a, &ctrl_k(), &tx, fake_restart));
    assert!(!a.proposal_pinned);
    let mut g = app_with_ghost("yes");
    assert!(!handle_key_with(&mut g, &ctrl_k(), &tx, fake_restart));
    assert!(!g.proposal_pinned);
}

#[test]
fn a_new_chip_is_unpinned() {
    let mut a = app_with(shell_chip());
    a.proposal_pinned = true;
    offer(&mut a, restart_chip());
    assert!(!a.proposal_pinned);
}
