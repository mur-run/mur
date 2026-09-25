//! `replay_with` + `--heal`: error classification (plan 5.5a) and the D3
//! verify / roll-back state machine (plan 5.5).

use super::*;
use crate::recorder::from_yaml;

/// Scripted caller with a fixed snapshot; tools in `fail` answer `isError`.
struct Page {
    snapshot: &'static str,
    fail: &'static [&'static str],
    calls: Vec<(String, Value)>,
}

impl Page {
    fn new(snapshot: &'static str) -> Self {
        Self {
            snapshot,
            fail: &[],
            calls: Vec::new(),
        }
    }

    fn sent(&self, tool: &str) -> Vec<&Value> {
        self.calls
            .iter()
            .filter(|(n, _)| n == tool)
            .map(|(_, a)| a)
            .collect()
    }
}

impl ToolCaller for Page {
    async fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        self.calls.push((name.to_owned(), arguments));
        let text = if name == "browser_snapshot" {
            self.snapshot
        } else {
            "ok"
        };
        let is_error = self.fail.contains(&name);
        Ok(json!({"content": [{"type": "text", "text": text}], "isError": is_error}))
    }
}

const SUBMIT_INTENT: &str = "點擊送出按鈕";

const HEAL: ReplayOptions = ReplayOptions {
    heal: true,
    ..ReplayOptions::DEFAULT
};

/// The order page after a copy change: 送出 → 送出訂單, 取消 → 取消訂單.
const ORDER_PAGE: &str = "- button \"送出訂單\" [ref=e2]\n- button \"取消訂單\" [ref=e3]\n- button \"返回\" [ref=e4]\n- heading \"訂單\" [ref=e5]";

fn steps(body: &str) -> Run {
    steps_in("test", body)
}

fn steps_in(mode: &str, body: &str) -> Run {
    from_yaml(&format!(
        "name: order\nmode: {mode}\nrecorded_at: 2026-09-24T00:00:00Z\nsteps:\n{body}"
    ))
    .unwrap()
}

fn click(n: u32, intent: &str, locators: &str) -> String {
    format!("- step: {n}\n  intent: {intent}\n  action: click\n  locators: [{locators}]\n")
}

fn statuses(report: &ReplayReport) -> Vec<StepStatus> {
    report.steps.iter().map(|s| s.status).collect()
}

async fn replay(run: &Run, page: &mut Page) -> ReplayReport {
    replay_with(run, &[], HEAL, page).await.unwrap()
}

#[tokio::test]
async fn all_miss_heals_and_next_direct_hit_verifies() {
    let run = steps(&format!(
        "{}{}",
        click(1, SUBMIT_INTENT, r#"'role:button[name="送出"]'"#),
        click(2, "點擊返回", r#"'role:button[name="返回"]'"#),
    ));
    let mut page = Page::new(ORDER_PAGE);
    let report = replay(&run, &mut page).await;

    assert_eq!(statuses(&report), [StepStatus::Healed, StepStatus::Passed]);
    assert_eq!(page.sent("browser_click")[0]["target"], "e2");
    assert_eq!(report.heals.len(), 1);
    let event = &report.heals[0];
    assert_eq!(event.status, HealStatus::Verified);
    assert_eq!(event.from, [r#"role:button[name="送出"]"#]);
    assert_eq!(event.to[0], r#"role:button[name="送出訂單"]"#);
    assert!(
        event.to.iter().all(|l| !l.starts_with('@')),
        "{:?}",
        event.to
    );
    assert_eq!(report.verdict(), "yellow");
}

#[tokio::test]
async fn heal_is_off_by_default() {
    let run = steps(&click(1, SUBMIT_INTENT, r#"'role:button[name="送出"]'"#));
    let mut page = Page::new(ORDER_PAGE);
    let report = replay_with(&run, &[], ReplayOptions::default(), &mut page)
        .await
        .unwrap();
    assert_eq!(statuses(&report), [StepStatus::Failed]);
    assert!(report.heals.is_empty());
    assert!(page.sent("browser_click").is_empty());
}

#[tokio::test]
async fn testid_fallback_error_fails_without_heal_and_acts_once() {
    let run = steps(&click(
        1,
        SUBMIT_INTENT,
        r#"'role:button[name="送出"]', 'testid:submit'"#,
    ));
    let mut page = Page {
        fail: &["browser_click"],
        ..Page::new(ORDER_PAGE)
    };
    let report = replay(&run, &mut page).await;

    assert_eq!(statuses(&report), [StepStatus::Failed]);
    assert!(report.heals.is_empty(), "{:?}", report.heals);
    let clicks = page.sent("browser_click");
    assert_eq!(clicks.len(), 1);
    assert_eq!(clicks[0]["target"], "[data-testid=\"submit\"]");
}

#[tokio::test]
async fn action_error_after_snapshot_hit_fails_without_heal() {
    let run = steps(&click(1, "點擊返回", r#"'role:button[name="返回"]'"#));
    let mut page = Page {
        fail: &["browser_click"],
        ..Page::new(ORDER_PAGE)
    };
    let report = replay(&run, &mut page).await;
    assert_eq!(statuses(&report), [StepStatus::Failed]);
    assert!(report.heals.is_empty());
    assert_eq!(page.sent("browser_click").len(), 1);
}

#[tokio::test]
async fn second_heal_in_a_row_rolls_back_the_first() {
    let run = steps(&format!(
        "{}{}{}",
        click(1, SUBMIT_INTENT, r#"'role:button[name="送出"]'"#),
        click(2, "點擊取消按鈕", r#"'role:button[name="取消"]'"#),
        click(3, "點擊返回", r#"'role:button[name="返回"]'"#),
    ));
    let mut page = Page::new(ORDER_PAGE);
    let report = replay(&run, &mut page).await;

    assert_eq!(
        statuses(&report),
        [StepStatus::Failed, StepStatus::Failed, StepStatus::Skipped]
    );
    let msg = report.steps[0].message.as_deref().unwrap();
    assert!(
        msg.contains("送出訂單") && msg.contains("rolled back"),
        "{msg}"
    );
    assert!(
        report
            .heals
            .iter()
            .all(|h| h.status == HealStatus::RolledBack),
        "{:?}",
        report.heals
    );
    assert_eq!(report.verdict(), "red");
}

#[tokio::test]
async fn failed_assert_text_between_rolls_back_the_heal() {
    let run = steps(&format!(
        "{}- step: 2\n  intent: 確認標題\n  action: assert_text\n  value: 訂單\n  locators: ['text:訂單']\n{}",
        click(1, SUBMIT_INTENT, r#"'role:button[name="送出"]'"#),
        click(3, "點擊返回", r#"'role:button[name="返回"]'"#),
    ));
    let mut page = Page {
        fail: &["browser_verify_text_visible"],
        ..Page::new(ORDER_PAGE)
    };
    let report = replay(&run, &mut page).await;

    assert_eq!(
        statuses(&report),
        [StepStatus::Failed, StepStatus::Failed, StepStatus::Skipped]
    );
    assert_eq!(report.heals[0].status, HealStatus::RolledBack);
}

#[tokio::test]
async fn passing_assert_text_does_not_verify_the_heal() {
    let run = steps(&format!(
        "{}- step: 2\n  intent: 確認標題\n  action: assert_text\n  value: 訂單\n  locators: ['text:訂單']\n",
        click(1, SUBMIT_INTENT, r#"'role:button[name="送出"]'"#),
    ));
    let mut page = Page::new(ORDER_PAGE);
    let report = replay(&run, &mut page).await;
    assert_eq!(statuses(&report), [StepStatus::Healed, StepStatus::Passed]);
    assert_eq!(report.heals[0].status, HealStatus::Unverified);
}

#[tokio::test]
async fn heal_on_last_element_step_is_unverified_and_passes() {
    let run = steps(&click(1, SUBMIT_INTENT, r#"'role:button[name="送出"]'"#));
    let mut page = Page::new(ORDER_PAGE);
    let report = replay(&run, &mut page).await;
    assert_eq!(report.failed, 0);
    assert_eq!(report.heals[0].status, HealStatus::Unverified);
}

#[tokio::test]
async fn declined_heal_reports_why() {
    let run = steps(&click(1, "點擊登出", r#"'role:link[name="登出"]'"#));
    let mut page = Page::new(ORDER_PAGE);
    let report = replay(&run, &mut page).await;
    assert_eq!(statuses(&report), [StepStatus::Failed]);
    let msg = report.steps[0].message.as_deref().unwrap();
    assert!(
        msg.contains("heal declined") && msg.contains("link"),
        "{msg}"
    );
    assert!(report.heals.is_empty());
}

#[tokio::test]
async fn assert_text_is_never_healed() {
    let run = steps(
        "- step: 1\n  intent: 確認送出\n  action: assert_text\n  value: 送出\n  locators: ['role:button[name=\"送出\"]']\n",
    );
    let mut page = Page::new(ORDER_PAGE);
    let report = replay(&run, &mut page).await;
    assert_eq!(statuses(&report), [StepStatus::Failed]);
    assert!(report.heals.is_empty());
}

/// Two verified heals over four element steps: allowed max(1, ⌊4 × 0.2⌋) = 1.
fn two_heals_in_four() -> String {
    format!(
        "{}{}{}{}",
        click(1, SUBMIT_INTENT, r#"'role:button[name="送出"]'"#),
        click(2, "點擊返回", r#"'role:button[name="返回"]'"#),
        click(3, "點擊取消按鈕", r#"'role:button[name="取消"]'"#),
        click(4, "點擊返回", r#"'role:button[name="返回"]'"#),
    )
}

#[tokio::test]
async fn test_mode_over_budget_is_red_but_still_ok() {
    let run = steps(&two_heals_in_four());
    let mut page = Page::new(ORDER_PAGE);
    let report = replay(&run, &mut page).await;

    assert_eq!(report.failed, 0);
    assert!(
        report
            .heals
            .iter()
            .all(|h| h.status == HealStatus::Verified),
        "{:?}",
        report.heals
    );
    let over = report.budget_exceeded.expect("over budget");
    assert_eq!((over.healed, over.total, over.allowed), (2, 4, 1));
    assert_eq!(report.verdict(), "red");
    assert!(report.summary().starts_with("red "), "{}", report.summary());
    assert_eq!(
        over.to_string(),
        "heal rate too high: 2 of 4 element steps healed (allowed 1 at --max-heal-ratio 0.2); the recording is stale — re-record it"
    );
}

#[tokio::test]
async fn automation_mode_ignores_the_budget() {
    let run = steps_in("automation", &two_heals_in_four());
    let mut page = Page::new(ORDER_PAGE);
    let report = replay(&run, &mut page).await;
    assert_eq!(report.heals.len(), 2);
    assert!(report.budget_exceeded.is_none());
    assert_eq!(report.verdict(), "yellow");
}

#[tokio::test]
async fn test_mode_within_raised_ratio_is_yellow() {
    let run = steps(&two_heals_in_four());
    let mut page = Page::new(ORDER_PAGE);
    let opts = ReplayOptions {
        max_heal_ratio: 0.5,
        ..HEAL
    };
    let report = replay_with(&run, &[], opts, &mut page).await.unwrap();
    assert!(report.budget_exceeded.is_none());
    assert_eq!(report.verdict(), "yellow");
}

#[tokio::test]
async fn rolled_back_heals_do_not_count_against_the_budget() {
    // Step 1 heals, step 2 needs a heal too → both rolled back, run stops.
    let run = steps(&format!(
        "{}{}",
        click(1, SUBMIT_INTENT, r#"'role:button[name="送出"]'"#),
        click(2, "點擊取消按鈕", r#"'role:button[name="取消"]'"#),
    ));
    let mut page = Page::new(ORDER_PAGE);
    let report = replay(&run, &mut page).await;
    assert!(report.failed > 0);
    assert!(report.budget_exceeded.is_none());
}

#[tokio::test]
async fn assert_text_steps_are_not_in_the_budget_denominator() {
    // 4 element steps + 6 asserts: counting all 10 steps would allow 2 heals.
    let asserts: String = (5..=10)
        .map(|n| {
            format!("- step: {n}\n  intent: 確認標題\n  action: assert_text\n  value: 訂單\n  locators: ['text:訂單']\n")
        })
        .collect();
    let run = steps(&format!("{}{asserts}", two_heals_in_four()));
    let mut page = Page::new(ORDER_PAGE);
    let report = replay(&run, &mut page).await;
    let over = report.budget_exceeded.expect("over budget");
    assert_eq!((over.total, over.allowed), (4, 1));
}
