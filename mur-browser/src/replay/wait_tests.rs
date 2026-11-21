//! `replay_with` waiting on assert steps: an assert is re-checked until it
//! holds or the timeout is up, and nothing that acts on the page is repeated.
//!
//! The clock is paused (`start_paused`), so a full timeout costs no real time
//! and elapsed time is exact.
use super::*;
use crate::recorder::from_yaml;
use std::collections::VecDeque;
use tokio::time::Instant;

const BUTTON: &str = "- button \"Save\" [ref=e7]";
const EMPTY: &str = "- heading \"Loading\" [ref=e1]";

/// Scripted page: each tool answers from its own queue, then repeats its
/// last answer for ever.
#[derive(Default)]
struct Page {
    /// `browser_snapshot` texts, in order.
    snapshots: VecDeque<&'static str>,
    /// Whether each call of the step's own tool is refused (`isError`).
    refusals: VecDeque<bool>,
    /// Fail the transport (an `Err`, not `isError`) for this tool.
    broken: Option<&'static str>,
    calls: Vec<String>,
}

impl Page {
    fn count(&self, tool: &str) -> usize {
        self.calls.iter().filter(|c| *c == tool).count()
    }
}

/// Pop the next scripted answer, keeping the last one in place.
fn next<T: Copy>(queue: &mut VecDeque<T>) -> Option<T> {
    if queue.len() > 1 {
        queue.pop_front()
    } else {
        queue.front().copied()
    }
}

impl ToolCaller for Page {
    async fn call_tool(&mut self, name: &str, _arguments: Value) -> Result<Value> {
        self.calls.push(name.to_owned());
        if self.broken == Some(name) {
            bail!("MCP server exited before answering tools/call");
        }
        let (text, is_error) = if name == "browser_snapshot" {
            (next(&mut self.snapshots).unwrap_or(BUTTON), false)
        } else {
            ("Text not found", next(&mut self.refusals).unwrap_or(false))
        };
        Ok(json!({"content": [{"type": "text", "text": text}], "isError": is_error}))
    }
}

fn one(mode: &str, body: &str) -> Run {
    from_yaml(&format!(
        "name: wait\nmode: {mode}\nrecorded_at: 2026-11-21T00:00:00Z\nsteps:\n- step: 1\n  intent: 測試步驟\n{body}"
    ))
    .unwrap()
}

const ASSERT_TEXT: &str = "  action: assert_text\n  value: 待我審核\n";
const ASSERT_VISIBLE: &str =
    "  action: assert_visible\n  locators: ['role:button[name=\"Save\"]']\n";
const CLICK: &str = "  action: click\n  locators: ['role:button[name=\"Save\"]']\n";

async fn replay(run: &Run, opts: ReplayOptions, page: &mut Page) -> (ReplayReport, Duration) {
    let started = Instant::now();
    let report = replay_with(run, &[], opts, page).await.unwrap();
    (report, started.elapsed())
}

/// The reported bug: `goto` returned, the SPA had not drawn yet, and the
/// first `browser_verify_text_visible` answered "Text not found".
#[tokio::test(start_paused = true)]
async fn assert_text_passes_once_the_page_has_rendered() {
    let mut page = Page {
        refusals: [true, true, false].into(),
        ..Page::default()
    };
    let (report, took) = replay(&one("test", ASSERT_TEXT), ReplayOptions::DEFAULT, &mut page).await;

    assert_eq!(report.verdict(), "green", "{report:#?}");
    assert_eq!(page.count("browser_verify_text_visible"), 3);
    assert_eq!(took, ASSERT_POLL_INTERVAL * 2);
}

#[tokio::test(start_paused = true)]
async fn assert_text_that_never_holds_fails_after_the_full_timeout() {
    let mut page = Page {
        refusals: [true].into(),
        ..Page::default()
    };
    let (report, took) = replay(&one("test", ASSERT_TEXT), ReplayOptions::DEFAULT, &mut page).await;

    assert_eq!(report.verdict(), "red");
    assert_eq!(took, DEFAULT_ASSERT_TIMEOUT);
    let message = report.steps[0].message.as_deref().unwrap();
    // Says it waited, and keeps the page's own reason.
    assert!(message.contains("after waiting 5s"), "{message}");
    assert!(message.contains("Text not found"), "{message}");
}

#[tokio::test(start_paused = true)]
async fn assert_visible_waits_for_its_element_and_is_not_healed() {
    // Heal on: a missing element before the timeout is a page still
    // rendering, not a stale locator, so it must not be healed.
    let mut page = Page {
        snapshots: [EMPTY, EMPTY, BUTTON].into(),
        ..Page::default()
    };
    let opts = ReplayOptions {
        heal: true,
        ..ReplayOptions::DEFAULT
    };
    let (report, took) = replay(&one("test", ASSERT_VISIBLE), opts, &mut page).await;

    assert_eq!(report.steps[0].status, StepStatus::Passed, "{report:#?}");
    assert!(report.heals.is_empty(), "{:?}", report.heals);
    assert_eq!(page.count("browser_snapshot"), 3);
    assert_eq!(page.count("browser_verify_element_visible"), 1);
    assert_eq!(took, ASSERT_POLL_INTERVAL * 2);
}

/// Waiting must never repeat something that acts on the page.
#[tokio::test(start_paused = true)]
async fn a_refused_click_is_sent_once_and_not_waited_on() {
    let mut page = Page {
        refusals: [true].into(),
        ..Page::default()
    };
    let (report, took) = replay(&one("test", CLICK), ReplayOptions::DEFAULT, &mut page).await;

    assert_eq!(report.steps[0].status, StepStatus::Failed);
    assert_eq!(page.count("browser_click"), 1);
    assert_eq!(took, Duration::ZERO);
    let message = report.steps[0].message.as_deref().unwrap();
    assert!(!message.contains("after waiting"), "{message}");
}

#[tokio::test(start_paused = true)]
async fn a_click_whose_locator_misses_fails_at_once() {
    let mut page = Page {
        snapshots: [EMPTY].into(),
        ..Page::default()
    };
    let (report, took) = replay(&one("test", CLICK), ReplayOptions::DEFAULT, &mut page).await;

    assert_eq!(report.steps[0].status, StepStatus::Failed);
    assert_eq!(page.count("browser_snapshot"), 1);
    assert_eq!(page.count("browser_click"), 0);
    assert_eq!(took, Duration::ZERO);
}

/// A dead server answers the same way every time: fail now, not in 5s.
#[tokio::test(start_paused = true)]
async fn a_transport_error_on_an_assert_is_not_waited_on() {
    let mut page = Page {
        broken: Some("browser_verify_text_visible"),
        ..Page::default()
    };
    let (report, took) = replay(&one("test", ASSERT_TEXT), ReplayOptions::DEFAULT, &mut page).await;

    assert_eq!(report.steps[0].status, StepStatus::Failed);
    assert_eq!(page.count("browser_verify_text_visible"), 1);
    assert_eq!(took, Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn zero_timeout_looks_once_and_keeps_the_plain_message() {
    let mut page = Page {
        refusals: [true, false].into(),
        ..Page::default()
    };
    let opts = ReplayOptions {
        assert_timeout: Duration::ZERO,
        ..ReplayOptions::DEFAULT
    };
    let (report, took) = replay(&one("test", ASSERT_TEXT), opts, &mut page).await;

    assert_eq!(report.steps[0].status, StepStatus::Failed);
    assert_eq!(page.count("browser_verify_text_visible"), 1);
    assert_eq!(took, Duration::ZERO);
    let message = report.steps[0].message.as_deref().unwrap();
    assert!(!message.contains("after waiting"), "{message}");
}

/// Automation still does not enforce an assert, but it waits first: the
/// page gets the same chance to render before the step is passed over.
#[tokio::test(start_paused = true)]
async fn automation_waits_before_passing_over_a_failed_assert() {
    let mut page = Page {
        refusals: [true].into(),
        ..Page::default()
    };
    let (report, took) = replay(
        &one("automation", ASSERT_TEXT),
        ReplayOptions::DEFAULT,
        &mut page,
    )
    .await;

    assert_eq!(report.steps[0].status, StepStatus::Skipped);
    assert_eq!(report.failed, 0);
    assert_eq!(took, DEFAULT_ASSERT_TIMEOUT);
}

/// A stale locator on an assert is still healed, at the last look.
#[tokio::test(start_paused = true)]
async fn an_assert_whose_locator_went_stale_is_healed_at_the_last_look() {
    let mut page = Page {
        snapshots: ["- button \"Save changes\" [ref=e7]"].into(),
        ..Page::default()
    };
    let opts = ReplayOptions {
        heal: true,
        ..ReplayOptions::DEFAULT
    };
    let run = one(
        "test",
        "  action: assert_visible\n  locators: ['role:button[name=\"Save\"]']\n",
    );
    let (report, took) = replay(&run, opts, &mut page).await;

    assert_eq!(report.steps[0].status, StepStatus::Healed, "{report:#?}");
    assert_eq!(report.heals.len(), 1);
    assert_eq!(took, DEFAULT_ASSERT_TIMEOUT);
}
