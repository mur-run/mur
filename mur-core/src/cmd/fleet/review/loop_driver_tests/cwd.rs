//! #1777: a review session sends its working directory as `context.cwd` on
//! every turn — main, reviewer, and a malformed-reply resend — and records
//! it in `session_started`, so members work in the repository the session
//! was started from instead of their own home.

use super::*;

/// Records `params["context"]["cwd"]` of every send, in order.
struct CwdProbe {
    seen: Mutex<Vec<(String, serde_json::Value)>>,
    reviewer: Mutex<Vec<String>>,
}

impl ReviewTransport for CwdProbe {
    fn send(&self, member: &str, params: &serde_json::Value) -> anyhow::Result<String> {
        let cwd = params["context"]["cwd"].clone();
        self.seen.lock().unwrap().push((member.to_string(), cwd));
        Ok(match member {
            "main" => with_accept_all("done", params),
            _ => self.reviewer.lock().unwrap().pop().unwrap(),
        })
    }
}

fn probe(reviewer_replies: Vec<&str>) -> CwdProbe {
    CwdProbe {
        seen: Mutex::new(Vec::new()),
        reviewer: Mutex::new(
            reviewer_replies
                .into_iter()
                .rev()
                .map(String::from)
                .collect(),
        ),
    }
}

fn run(t: &CwdProbe, cwd: Option<&str>) -> (tempfile::TempDir, String, LoopDriverStop) {
    let (tmp, channel_id) = setup_channel();
    let (_, stop) = run_review_loop(
        t,
        tmp.path(),
        "review-cwd",
        &channel_id,
        "main",
        "reviewer",
        "task",
        Mode::SemiAuto,
        Duration::ZERO,
        SessionLimits::new(Duration::from_secs(3600), Stuck::Off, None),
        &Instant::now,
        cwd,
    )
    .unwrap();
    (tmp, channel_id, stop)
}

#[test]
fn every_send_carries_the_session_cwd() {
    let revise = serde_json::json!({
        "verdict": "revise",
        "findings": [{"severity": "low", "issue": "x"}],
    })
    .to_string();
    let approve = serde_json::json!({
        "verdict": "approve",
        "prior": [{"id": "F1", "status": "resolved"}],
    })
    .to_string();
    // "not json" forces a malformed-reply resend to the reviewer.
    let t = probe(vec!["not json", &revise, &approve]);
    let (_tmp, _, stop) = run(&t, Some("/work/repo"));
    assert_eq!(stop, LoopDriverStop::Approve);

    let seen = t.seen.lock().unwrap();
    assert_eq!(
        seen.len(),
        5,
        "main, reviewer, resend, main, reviewer: {seen:?}"
    );
    for (member, cwd) in seen.iter() {
        assert_eq!(cwd, "/work/repo", "send to {member} lost the cwd");
    }
}

#[test]
fn session_started_records_the_cwd() {
    let approve = serde_json::json!({"verdict": "approve"}).to_string();
    let t = probe(vec![&approve]);
    let (tmp, channel_id, _) = run(&t, Some("/work/repo"));
    let started = read_payloads(tmp.path(), &channel_id)
        .into_iter()
        .find_map(|p| match p {
            ReviewPayload::SessionStarted { cwd, .. } => Some(cwd),
            _ => None,
        })
        .unwrap();
    assert_eq!(started.as_deref(), Some("/work/repo"));
}

#[test]
fn no_cwd_sends_no_context_cwd() {
    let approve = serde_json::json!({"verdict": "approve"}).to_string();
    let t = probe(vec![&approve]);
    let _ = run(&t, None);
    for (member, cwd) in t.seen.lock().unwrap().iter() {
        assert!(cwd.is_null(), "send to {member} carried {cwd}");
    }
}
