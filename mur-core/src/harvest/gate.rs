//! Heuristic harvest gate — zero-token port of `session_worth_analyzing`
//! (spec §3.2: gate defaults to heuristics; LLM gating is a later, optional
//! enhancement). Marked sessions pass unconditionally (§3.3).

use mur_common::config::HarvestCfg;

use crate::session::{SessionEvent, SessionMeta};

pub struct GateDecision {
    pub pass: bool,
    pub reason: String,
}

/// Substrings that identify mur's own bookkeeping noise in event content.
const NOISE_MARKERS: &[&str] = &[
    "mur session",
    "mur sync",
    "mur context",
    "mur inject",
    "/mur:in",
    "/mur:out",
    "/mur-in",
    "/mur-out",
    "[stop:",
    "turn_end",
];

pub fn gate(events: &[SessionEvent], meta: &SessionMeta, cfg: &HarvestCfg) -> GateDecision {
    if meta.marked {
        return GateDecision {
            pass: true,
            reason: "marked via mur in".to_string(),
        };
    }

    let non_noise = events
        .iter()
        .filter(|e| {
            let lower = e.content.to_lowercase();
            !NOISE_MARKERS.iter().any(|n| lower.contains(n))
        })
        .count();
    let tool_calls = events
        .iter()
        .filter(|e| e.event_type == "tool_call")
        .count();
    let duration_secs = match (events.first(), events.last()) {
        (Some(f), Some(l)) if l.timestamp >= f.timestamp => {
            ((l.timestamp - f.timestamp) / 1000) as i64
        }
        _ => 0,
    };

    // Conservative: skip only when ALL signals are below threshold
    // (same shape as session_worth_analyzing, thresholds from config).
    if non_noise < cfg.min_events
        && meta.user_turns < cfg.min_user_turns
        && duration_secs < cfg.min_duration_secs
        && tool_calls == 0
    {
        return GateDecision {
            pass: false,
            reason: format!(
                "{} events, {} turns, {}s, {} tool calls",
                non_noise, meta.user_turns, duration_secs, tool_calls
            ),
        };
    }
    GateDecision {
        pass: true,
        reason: String::new(),
    }
}

/// Second gate, on the extracted step list: reject recordings whose *shape* says
/// "session" rather than "procedure" (#781).
///
/// `gate` above is floor-only — a single tool call clears it — so a 405-step,
/// 24-hour debugging transcript passed as easily as a five-command deploy. Those
/// proposals are unreviewable, and an inbox full of them buries the good ones.
/// A session marked with `mur in` is an explicit human "yes" and bypasses this.
pub fn shape_gate(
    steps: &[String],
    duration_secs: i64,
    marked: bool,
    cfg: &HarvestCfg,
) -> GateDecision {
    if marked {
        return GateDecision {
            pass: true,
            reason: "marked via mur in".to_string(),
        };
    }
    if steps.len() > cfg.max_steps {
        return GateDecision {
            pass: false,
            reason: format!("{} steps > max_steps {}", steps.len(), cfg.max_steps),
        };
    }
    if duration_secs > cfg.max_duration_secs {
        return GateDecision {
            pass: false,
            reason: format!(
                "{}s > max_duration_secs {}",
                duration_secs, cfg.max_duration_secs
            ),
        };
    }
    GateDecision {
        pass: true,
        reason: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(user_turns: usize, marked: bool) -> SessionMeta {
        SessionMeta {
            id: "s".into(),
            source: "claude".into(),
            started_at: "2026-01-01T00:00:00Z".into(),
            stopped_at: None,
            title: None,
            tools_used: vec![],
            user_turns,
            assistant_turns: 0,
            marked,
            gated_at: None,
            harvested_at: None,
        }
    }

    fn event(ts: u64, event_type: &str, content: &str) -> SessionEvent {
        SessionEvent {
            timestamp: ts,
            event_type: event_type.into(),
            tool: None,
            content: content.into(),
            ..Default::default()
        }
    }

    #[test]
    fn tiny_session_fails_gate() {
        let cfg = HarvestCfg::default();
        let events = vec![event(0, "user", "hi")];
        assert!(!gate(&events, &meta(1, false), &cfg).pass);
    }

    #[test]
    fn tool_heavy_session_passes_gate() {
        let cfg = HarvestCfg::default();
        let events = vec![
            event(0, "user", "deploy"),
            event(1000, "tool_call", r#"{"command":"fly deploy"}"#),
        ];
        assert!(gate(&events, &meta(1, false), &cfg).pass);
    }

    #[test]
    fn marked_session_always_passes() {
        let cfg = HarvestCfg::default();
        assert!(gate(&[], &meta(0, true), &cfg).pass);
    }

    fn steps(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("step-{i}")).collect()
    }

    #[test]
    fn short_session_passes_shape_gate() {
        let cfg = HarvestCfg::default();
        assert!(shape_gate(&steps(5), 60, false, &cfg).pass);
    }

    #[test]
    fn long_session_fails_shape_gate() {
        let cfg = HarvestCfg::default();
        let d = shape_gate(&steps(cfg.max_steps + 1), 60, false, &cfg);
        assert!(!d.pass);
        assert!(d.reason.contains("max_steps"), "{}", d.reason);
    }

    #[test]
    fn slow_session_fails_shape_gate() {
        let cfg = HarvestCfg::default();
        let d = shape_gate(&steps(3), cfg.max_duration_secs + 1, false, &cfg);
        assert!(!d.pass);
        assert!(d.reason.contains("max_duration_secs"), "{}", d.reason);
    }

    #[test]
    fn marked_session_bypasses_shape_gate() {
        let cfg = HarvestCfg::default();
        let long = steps(cfg.max_steps + 100);
        assert!(shape_gate(&long, cfg.max_duration_secs * 10, true, &cfg).pass);
    }
}
