//! Tests for `loop_driver.rs` (D3, AC12): "a full loop runs to `approve`"
//! (spec line 511), built on the EXISTING driver (`run_turn_with_retry`) and
//! ledger (`fold`, `apply`, `note_round_complete`) rather than inventing a
//! second way to send or fold.

mod channel;
mod flow;
mod guards;
mod malformed;

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use mur_common::channel::EventKind;
use mur_common::limits::Stuck;

use super::driver::ReviewTransport;
use super::ledger::fold_rounds;
use super::loop_driver::{LoopDriverStop, run_review_loop};
use super::schema::{
    Mode, NoteClassification, ReviewPayload, Role, SessionLimits, classify_note_payload,
};
use crate::cmd::fleet::loop_run::LoopStop;

/// A main reply as a well-behaved main agent would send it: `text`, plus —
/// when the prompt lists open findings (`- F<n> [...`) — a §3.4 rebuttal
/// accepting every one of them.
fn with_accept_all(text: &str, params: &serde_json::Value) -> String {
    let prompt = super::wire::message_text(params).unwrap_or_default();
    let responses: Vec<serde_json::Value> = prompt
        .lines()
        .filter_map(|l| l.strip_prefix("- "))
        .filter_map(|l| l.split_once(" ["))
        .map(|(id, _)| id)
        .filter(|id| id.starts_with('F'))
        .map(|id| serde_json::json!({"id": id, "answer": "accept"}))
        .collect();
    if responses.is_empty() {
        return text.to_string();
    }
    format!(
        "{text}\n```json\n{}\n```",
        serde_json::json!({ "responses": responses })
    )
}

/// Test-only transport: counts sends PER MEMBER and returns the next queued
/// reply for that member. Mirrors `driver_tests.rs`'s `StubTransport`, but
/// keyed by member name so a two-party loop can script `main` and
/// `reviewer` independently.
struct StubLoopTransport {
    main_sends: AtomicUsize,
    reviewer_sends: AtomicUsize,
    main_replies: Mutex<Vec<String>>,
    reviewer_replies: Mutex<Vec<String>>,
}

impl StubLoopTransport {
    fn new(main_replies: Vec<&str>, reviewer_replies: Vec<&str>) -> Self {
        let mut main = main_replies
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        main.reverse();
        let mut reviewer = reviewer_replies
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        reviewer.reverse();
        Self {
            main_sends: AtomicUsize::new(0),
            reviewer_sends: AtomicUsize::new(0),
            main_replies: Mutex::new(main),
            reviewer_replies: Mutex::new(reviewer),
        }
    }

    fn main_send_count(&self) -> usize {
        self.main_sends.load(Ordering::SeqCst)
    }

    fn reviewer_send_count(&self) -> usize {
        self.reviewer_sends.load(Ordering::SeqCst)
    }
}

impl ReviewTransport for StubLoopTransport {
    fn send(&self, member: &str, params: &serde_json::Value) -> anyhow::Result<String> {
        match member {
            "main" => {
                self.main_sends.fetch_add(1, Ordering::SeqCst);
                let text = self
                    .main_replies
                    .lock()
                    .unwrap()
                    .pop()
                    .unwrap_or_else(|| "ok".to_string());
                Ok(with_accept_all(&text, params))
            }
            "reviewer" => {
                self.reviewer_sends.fetch_add(1, Ordering::SeqCst);
                Ok(self
                    .reviewer_replies
                    .lock()
                    .unwrap()
                    .pop()
                    .unwrap_or_else(|| serde_json::json!({"verdict": "approve"}).to_string()))
            }
            other => panic!("unexpected member {other:?}"),
        }
    }
}

/// Stub transport for the stuck-detector tests below: a single scripted
/// reply per member, but it advances a shared fake clock by `jump` INSIDE
/// the reviewer's `send` — simulating a reviewer turn that itself takes
/// longer than the stuck window, before `run_review_loop` ever gets to look
/// at the reply.
struct StubClockTransport {
    clock: Rc<Cell<Instant>>,
    jump: Duration,
    main_reply: String,
    reviewer_reply: String,
}

impl ReviewTransport for StubClockTransport {
    fn send(&self, member: &str, params: &serde_json::Value) -> anyhow::Result<String> {
        match member {
            "main" => Ok(with_accept_all(&self.main_reply, params)),
            "reviewer" => {
                self.clock.set(self.clock.get() + self.jump);
                Ok(self.reviewer_reply.clone())
            }
            other => panic!("unexpected member {other:?}"),
        }
    }
}

/// Returns a fresh `~/.mur`-shaped tempdir with a review channel created and
/// the router's signing identity planted — copied from
/// `driver_tests.rs::setup_channel` (same shape the retry/pause path needs).
fn setup_channel() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    crate::channel_writer::plant_writer_identity(home);
    let svc = mur_channel::ChannelService::open(home).unwrap();
    let channel_id = "review-ac12-channel".to_string();
    svc.store()
        .create(&mur_common::channel::Channel {
            v: mur_common::channel::CHANNEL_SCHEMA_VERSION,
            id: channel_id.clone(),
            title: "t".into(),
            goal: mur_common::channel::Goal::default(),
            state: mur_common::channel::ChannelState::Working,
            purpose: None,
            owner: mur_common::channel::ChannelActor::System,
            participants: vec![],
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        })
        .unwrap();
    (tmp, channel_id)
}

/// Read back every review payload on `channel_id`, in channel order — the
/// shape `fold` expects.
fn read_payloads(home: &std::path::Path, channel_id: &str) -> Vec<ReviewPayload> {
    let svc = mur_channel::ChannelService::open(home).unwrap();
    svc.load_events(channel_id)
        .unwrap()
        .into_iter()
        .filter(|ev| ev.kind == EventKind::Note)
        .filter_map(|ev| match classify_note_payload(&ev.payload) {
            NoteClassification::Review(env) => Some(env.payload),
            _ => None,
        })
        .collect()
}
