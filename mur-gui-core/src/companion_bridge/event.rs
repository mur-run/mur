//! Bridge event types + inbox markdown parser.
//!
//! `BridgeEvent` is the common over-the-wire shape between the Rust
//! bridge and the React UI.  It is `serde::Serialize` so it can ride
//! a Tauri 2 `Channel<BridgeEvent>`.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeEvent {
    pub id: String,
    pub situation: String,
    pub template_id: String,
    pub locale: String,
    pub generated_at: String,
    pub body: String,
    pub response: BridgeResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum BridgeResponse {
    Unset,
    Signal(String),
}

#[derive(Deserialize)]
struct FrontMatter {
    id: String,
    situation: String,
    template_id: String,
    locale: String,
    generated_at: String,
}

pub fn parse_inbox_md(path: &Path) -> Result<BridgeEvent> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    parse_str(&raw)
}

pub fn parse_str(raw: &str) -> Result<BridgeEvent> {
    let stripped = raw
        .strip_prefix("---\n")
        .ok_or_else(|| anyhow!("missing opening front-matter fence"))?;
    let (yaml, rest) = stripped
        .split_once("\n---\n")
        .ok_or_else(|| anyhow!("missing closing front-matter fence"))?;
    let fm: FrontMatter = serde_yaml_ng::from_str(yaml).context("parse front-matter")?;

    let response_marker = ">>> response:";
    let (body_block, response_line) = rest
        .rsplit_once(response_marker)
        .ok_or_else(|| anyhow!("missing response marker"))?;
    let body = body_block.trim().to_string();
    let response_value = response_line.trim();
    let response = if response_value == "<unset>" {
        BridgeResponse::Unset
    } else if matches!(response_value, "good" | "bad" | "dismiss" | "snooze") {
        BridgeResponse::Signal(response_value.to_string())
    } else {
        bail!("unrecognized response value: {response_value}");
    };

    Ok(BridgeEvent {
        id: fm.id,
        situation: fm.situation,
        template_id: fm.template_id,
        locale: fm.locale,
        generated_at: fm.generated_at,
        body,
        response,
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\nid: abc-123\nsituation: test\ntemplate_id: t1\nlocale: en\ngenerated_at: 2026-05-14T00:00:00Z\n---\n\nHello world\n\n>>> response: <unset>";

    #[test]
    fn parses_unset_response() {
        let ev = parse_str(SAMPLE).unwrap();
        assert_eq!(ev.id, "abc-123");
        assert_eq!(ev.body, "Hello world");
        assert_eq!(ev.response, BridgeResponse::Unset);
    }

    #[test]
    fn parses_good_signal() {
        let s = SAMPLE.replace("<unset>", "good");
        let ev = parse_str(&s).unwrap();
        assert_eq!(ev.response, BridgeResponse::Signal("good".into()));
    }

    #[test]
    fn parses_snooze_response() {
        let s = SAMPLE.replace("<unset>", "snooze");
        let ev = parse_str(&s).unwrap();
        assert_eq!(ev.response, BridgeResponse::Signal("snooze".into()));
    }

    #[test]
    fn rejects_missing_fence() {
        assert!(parse_str("no fence here").is_err());
    }
}
