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
