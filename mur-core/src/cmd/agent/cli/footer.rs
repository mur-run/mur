//! Pure footer math: tokens, cost, and context-window fill from `Task.usage`
//! plus the agent's `models.yaml` pricing. No ratatui, no I/O — unit-tested.

use serde_json::Value;

/// Context bar thresholds (percent) and width.
pub const CTX_YELLOW_PCT: u8 = 70;
pub const CTX_RED_PCT: u8 = 90;
pub const CTX_BAR_WIDTH: usize = 6;

#[derive(Debug, Clone, Copy, Default)]
pub struct UsageCounts {
    pub input: u64,
    pub output: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Pricing {
    pub in_per_1k: Option<f64>,
    pub out_per_1k: Option<f64>,
    pub window: Option<u64>,
}

pub enum CtxColor {
    Green,
    Yellow,
    Red,
}

pub fn parse_usage(usage: &Value) -> UsageCounts {
    UsageCounts {
        input: usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

/// Clean per-context fill emitted by the runtime (Task 1). `None` on older
/// runtimes — the caller falls back to hiding the bar.
pub fn context_tokens(usage: &Value) -> Option<u64> {
    usage.get("context_tokens").and_then(Value::as_u64)
}

pub fn turn_cost(p: &Pricing, u: &UsageCounts) -> Option<f64> {
    match (p.in_per_1k, p.out_per_1k) {
        (Some(i), Some(o)) => Some(u.input as f64 / 1000.0 * i + u.output as f64 / 1000.0 * o),
        _ => None,
    }
}

pub fn context_pct(used: u64, window: u64) -> u8 {
    if window == 0 {
        return 0;
    }
    ((used as f64 / window as f64) * 100.0)
        .round()
        .clamp(0.0, 100.0) as u8
}

pub fn ctx_color(pct: u8) -> CtxColor {
    if pct < CTX_YELLOW_PCT {
        CtxColor::Green
    } else if pct < CTX_RED_PCT {
        CtxColor::Yellow
    } else {
        CtxColor::Red
    }
}

pub fn ctx_bar(pct: u8, width: usize) -> String {
    let filled = (pct as usize * width) / 100;
    format!("{}{}", "▓".repeat(filled), "░".repeat(width - filled))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usage_fields() {
        let u = parse_usage(&serde_json::json!({ "input_tokens": 1000, "output_tokens": 240 }));
        assert_eq!(u.input, 1000);
        assert_eq!(u.output, 240);
    }

    #[test]
    fn context_pct_is_input_over_window() {
        assert_eq!(context_pct(32_000, 100_000), 32);
        assert_eq!(context_pct(0, 100_000), 0);
        assert_eq!(context_pct(100, 0), 0); // no window → 0, never divide by zero
    }

    #[test]
    fn ctx_color_thresholds() {
        assert!(matches!(ctx_color(69), CtxColor::Green));
        assert!(matches!(ctx_color(70), CtxColor::Yellow));
        assert!(matches!(ctx_color(89), CtxColor::Yellow));
        assert!(matches!(ctx_color(90), CtxColor::Red));
    }

    #[test]
    fn cost_none_when_unpriced() {
        let u = UsageCounts {
            input: 1000,
            output: 1000,
        };
        let unpriced = Pricing {
            in_per_1k: None,
            out_per_1k: None,
            window: None,
        };
        assert!(turn_cost(&unpriced, &u).is_none());
        let priced = Pricing {
            in_per_1k: Some(0.003),
            out_per_1k: Some(0.015),
            window: Some(200_000),
        };
        let c = turn_cost(&priced, &u).unwrap();
        assert!((c - 0.018).abs() < 1e-9);
    }

    #[test]
    fn bar_fills_proportionally() {
        assert_eq!(ctx_bar(50, 6), "▓▓▓░░░");
        assert_eq!(ctx_bar(0, 6), "░░░░░░");
        assert_eq!(ctx_bar(100, 6), "▓▓▓▓▓▓");
    }
}
