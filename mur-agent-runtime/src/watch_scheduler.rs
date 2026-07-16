//! C-watch — proactive co-watching scene detection (spec §6). Runtime-only.
//!
//! Takes its own VLC snapshot over HTTP, computes a difference-hash (dHash), and on a
//! large change injects a short text turn into the `TaskRunner`. Cannot call `mur-core`
//! (would cycle), so it relies on `mur_common::media` shared types + the agent's own
//! `scene_explain` tool to actually narrate.

/// Hamming distance between two 64-bit perceptual hashes.
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// Compute a dHash from a row-major grayscale buffer of size `w`×`h`.
/// Sets one bit per horizontally-adjacent pair (`right > left`). For 9×8 ⇒ 64 bits.
pub fn dhash_from_luma(luma: &[u8], w: usize, h: usize) -> u64 {
    let mut hash = 0u64;
    let mut bit = 0u32;
    for y in 0..h {
        for x in 0..w.saturating_sub(1) {
            let left = luma[y * w + x];
            let right = luma[y * w + x + 1];
            if right > left {
                hash |= 1u64 << bit;
            }
            bit += 1;
        }
    }
    hash
}

use mur_common::media::Consent;

/// What the scheduler should do this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Narrate,
    AskConsent,
    Skip,
}

/// Pure gate — no I/O. Order of checks: mute → quiet → magnitude → cooldown → consent.
#[allow(clippy::too_many_arguments)]
pub fn should_interject(
    now_ms: i64,
    last_interjection_ms: i64,
    cooldown_ms: i64,
    distance: u32,
    threshold: u32,
    muted: bool,
    quiet_now: bool,
    consent: Consent,
) -> Decision {
    if muted || quiet_now {
        return Decision::Skip;
    }
    if distance < threshold {
        return Decision::Skip;
    }
    if last_interjection_ms != 0 && now_ms.saturating_sub(last_interjection_ms) < cooldown_ms {
        return Decision::Skip;
    }
    match consent {
        Consent::Unasked => Decision::AskConsent,
        Consent::Granted => Decision::Narrate,
        Consent::Declined => Decision::Skip,
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    const CD: i64 = 45_000;
    const TH: u32 = 18;

    #[test]
    fn small_change_skips() {
        assert_eq!(
            should_interject(100_000, 0, CD, 5, TH, false, false, Consent::Granted),
            Decision::Skip
        );
    }

    #[test]
    fn first_big_change_asks_consent() {
        assert_eq!(
            should_interject(100_000, 0, CD, 30, TH, false, false, Consent::Unasked),
            Decision::AskConsent
        );
    }

    #[test]
    fn granted_big_change_narrates() {
        assert_eq!(
            should_interject(100_000, 0, CD, 30, TH, false, false, Consent::Granted),
            Decision::Narrate
        );
    }

    #[test]
    fn cooldown_blocks() {
        // fired 10s ago, cooldown 45s ⇒ skip
        assert_eq!(
            should_interject(110_000, 100_000, CD, 30, TH, false, false, Consent::Granted),
            Decision::Skip
        );
    }

    #[test]
    fn muted_quiet_declined_all_skip() {
        assert_eq!(
            should_interject(200_000, 0, CD, 99, TH, true, false, Consent::Granted),
            Decision::Skip
        );
        assert_eq!(
            should_interject(200_000, 0, CD, 99, TH, false, true, Consent::Granted),
            Decision::Skip
        );
        assert_eq!(
            should_interject(200_000, 0, CD, 99, TH, false, false, Consent::Declined),
            Decision::Skip
        );
    }
}

#[cfg(test)]
mod hash_tests {
    use super::*;

    #[test]
    fn hamming_counts_differing_bits() {
        assert_eq!(hamming(0b0000, 0b0000), 0);
        assert_eq!(hamming(0b1010, 0b0001), 3);
    }

    #[test]
    fn dhash_detects_horizontal_gradient() {
        // 9x8 ascending rows ⇒ every pair has right > left ⇒ all 64 bits set.
        let mut luma = vec![0u8; 9 * 8];
        for y in 0..8 {
            for x in 0..9 {
                luma[y * 9 + x] = (x * 20) as u8;
            }
        }
        assert_eq!(dhash_from_luma(&luma, 9, 8), u64::MAX);

        // A flat image ⇒ no bit set ⇒ maximally different from the gradient.
        let flat = vec![100u8; 9 * 8];
        assert_eq!(dhash_from_luma(&flat, 9, 8), 0);
        assert_eq!(hamming(u64::MAX, 0), 64);
    }
}

use crate::llm::{BackgroundKind, RequestIntent};
use crate::task_runner::{TaskRunner, TaskSpec};
use chrono::{Local, Timelike};
use mur_common::a2a::{Message, MessagePart};
use mur_common::agent::QuietHours;
use mur_common::media::{VlcRuntime, load_runtime, load_watch, newest_file, save_watch};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::info;

const DEFAULT_POLL_SECS: u64 = 6;
const SCENE_CHANGE_THRESHOLD: u32 = 18;
const INTERJECTION_COOLDOWN_MS: i64 = 45_000;
const NARRATE_PROMPT: &str = "畫面剛切換了，看一下螢幕，用一句話簡短說你看到什麼。";
const CONSENT_PROMPT: &str =
    "我可以在劇情轉折時，偶爾插一句話幫你補充嗎？不想聽的話跟我說「噓」就好。";

pub struct WatchScheduler {
    runner: Arc<TaskRunner>,
    mur_home: PathBuf,
    quiet_hours: Option<QuietHours>,
    tick: Duration,
}

impl WatchScheduler {
    pub fn new(
        runner: Arc<TaskRunner>,
        mur_home: PathBuf,
        quiet_hours: Option<QuietHours>,
    ) -> Self {
        Self {
            runner,
            mur_home,
            quiet_hours,
            tick: Duration::from_secs(DEFAULT_POLL_SECS),
        }
    }

    pub fn with_tick(mut self, d: Duration) -> Self {
        self.tick = d;
        self
    }

    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        let cancel = CancellationToken::new();
        tokio::spawn(async move {
            let _g = cancel.clone().drop_guard();
            run_loop(self, cancel).await;
        })
    }
}

/// Ask VLC for a snapshot, read the resulting PNG, delete it, and return its bytes.
async fn capture_png(rt: &VlcRuntime, client: &reqwest::Client) -> Option<Vec<u8>> {
    let base = format!("http://127.0.0.1:{}/requests/status.xml", rt.port);
    let status = client
        .get(&base)
        .basic_auth("", Some(&rt.password))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    // Use the shared parser rather than a raw substring match, so pretty-printed
    // XML (`<state>\n playing\n</state>`) and nested same-named tags are handled
    // the same way as mur-core's tools.
    if mur_common::media::parse_status_xml(&status).state != "playing" {
        return None;
    }
    let _ = client
        .get(format!("{base}?command=snapshot"))
        .basic_auth("", Some(&rt.password))
        .timeout(Duration::from_secs(3))
        .send()
        .await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    let path = newest_file(&rt.snapshot_dir)?;
    let bytes = std::fs::read(&path).ok()?;
    let _ = std::fs::remove_file(&path); // lifecycle: never accumulate (spec §6.3)
    Some(bytes)
}

/// Decode PNG bytes → 9×8 grayscale → dHash.
fn dhash_png(bytes: &[u8]) -> Option<u64> {
    use image::imageops::FilterType;
    let img = image::load_from_memory(bytes).ok()?;
    let small = img.grayscale().resize_exact(9, 8, FilterType::Triangle);
    let luma = small.to_luma8();
    Some(dhash_from_luma(luma.as_raw(), 9, 8))
}

fn inject(runner: &Arc<TaskRunner>, text: &str) {
    let runner = runner.clone();
    let input = Message {
        role: "user".into(),
        parts: vec![MessagePart::Text { text: text.into() }],
    };
    tokio::spawn(async move {
        let _ = runner
            .run_sync(TaskSpec {
                input,
                context_task_id: None,
                task_id: None,
                active_fleet: None,
                active_team: None,
                // Co-watching narration is a runtime-initiated proactive nudge —
                // no live user turn is waiting on it.
                intent: RequestIntent::Background(BackgroundKind::Maintenance),
                output_artifact_path: None,
            })
            .await;
    });
}

/// Parse `"HH:MM"` into minutes-since-midnight.
fn parse_hhmm(s: &str) -> Option<i64> {
    let (h, m) = s.split_once(':')?;
    let h: i64 = h.trim().parse().ok()?;
    let m: i64 = m.trim().parse().ok()?;
    ((0..24).contains(&h) && (0..60).contains(&m)).then_some(h * 60 + m)
}

/// Is `now_min` (minutes since midnight) within the quiet window `[start, end)`,
/// handling midnight wrap (`start > end` means the window crosses midnight)?
fn in_quiet_window(now_min: i64, start: &str, end: &str) -> bool {
    let (Some(s), Some(e)) = (parse_hhmm(start), parse_hhmm(end)) else {
        return false;
    };
    if s == e {
        return false; // degenerate / empty window → never quiet
    }
    if s < e {
        now_min >= s && now_min < e
    } else {
        now_min >= s || now_min < e // wraps past midnight
    }
}

#[cfg(test)]
mod quiet_tests {
    use super::*;

    #[test]
    fn overnight_window_wraps_midnight() {
        // 22:00–08:00 quiet: 23:00 and 02:00 are quiet; 12:00 and 20:00 are not.
        assert!(in_quiet_window(23 * 60, "22:00", "08:00"));
        assert!(in_quiet_window(2 * 60, "22:00", "08:00"));
        assert!(!in_quiet_window(12 * 60, "22:00", "08:00"));
        assert!(!in_quiet_window(20 * 60, "22:00", "08:00"));
    }

    #[test]
    fn same_day_window_and_degenerate() {
        // 09:00–17:00 quiet: 12:00 quiet, 20:00 not.
        assert!(in_quiet_window(12 * 60, "09:00", "17:00"));
        assert!(!in_quiet_window(20 * 60, "09:00", "17:00"));
        // Empty/degenerate or unparseable → never quiet.
        assert!(!in_quiet_window(12 * 60, "08:00", "08:00"));
        assert!(!in_quiet_window(12 * 60, "bad", "08:00"));
    }
}

async fn run_loop(s: WatchScheduler, cancel: CancellationToken) {
    let client = reqwest::Client::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(s.tick) => {}
        }
        let session = load_watch(&s.mur_home);
        if !session.active {
            continue;
        }
        let Some(rt) = load_runtime(&s.mur_home) else {
            continue;
        };
        let Some(png) = capture_png(&rt, &client).await else {
            continue;
        };
        let Some(hash) = dhash_png(&png) else {
            continue;
        };

        let now_local = Local::now();
        let now_ms = now_local.timestamp_millis();
        let quiet_now = s
            .quiet_hours
            .as_ref()
            .map(|q| {
                let now_min = now_local.hour() as i64 * 60 + now_local.minute() as i64;
                in_quiet_window(now_min, &q.start, &q.end)
            })
            .unwrap_or(false);
        let distance = hamming(hash, session.last_scene_phash);

        let decision = should_interject(
            now_ms,
            session.last_interjection_ms,
            INTERJECTION_COOLDOWN_MS,
            distance,
            SCENE_CHANGE_THRESHOLD,
            session.muted,
            quiet_now,
            session.consent,
        );

        // Re-read just before acting/persisting: a concurrent watch_mute/watch_stop
        // (separate MCP process) during the multi-second capture must not be clobbered
        // (lost-mute/stop race). Only scheduler-owned fields are written back, and the
        // injection is re-gated on the fresh mute/active state.
        let mut fresh = load_watch(&s.mur_home);
        if !fresh.active {
            continue;
        }
        match decision {
            Decision::Narrate if !fresh.muted => {
                info!(distance, "watch: narrating scene change");
                inject(&s.runner, NARRATE_PROMPT);
                fresh.last_interjection_ms = now_ms;
            }
            Decision::AskConsent if !fresh.muted && fresh.consent == Consent::Unasked => {
                info!("watch: asking consent");
                inject(&s.runner, CONSENT_PROMPT);
                fresh.consent = Consent::Granted; // proceed next time; "噓" mutes.
                fresh.last_interjection_ms = now_ms;
            }
            _ => {}
        }
        fresh.last_scene_phash = hash; // always track the latest frame
        if let Err(e) = save_watch(&s.mur_home, &fresh) {
            // Persisting consent/cooldown/last_scene matters: a silent failure here
            // makes the scheduler re-ask consent and re-narrate every tick. Surface
            // it (e.g. a sandbox denial or full disk) rather than swallowing.
            tracing::warn!(error = %e, "watch: failed to persist co-watching session");
        }
    }
}
