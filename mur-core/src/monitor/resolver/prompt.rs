//! What the model is shown — spec §混合處置策略 step 3's "去敏後 spec、最近
//! observations、logs 摘要及已嘗試動作".
//!
//! Redaction happens HERE, on the way into the prompt, and not at the call
//! sites that assemble the context. One chokepoint is the only arrangement
//! that can be audited: with redaction at the callers, "did this field get
//! redacted?" becomes a question about every caller, forever, and the answer
//! is eventually no. `mur_common::redact` is the same chokepoint the hook
//! capture log writes through.

use mur_common::redact::{redact_home_path, redact_secrets};

/// How much log tail the model may see. A cap, not a preference: an
/// unbounded tail is both a cost and a disclosure surface, and the last few
/// hundred characters are where a build failure states itself.
pub const LOG_TAIL_MAX_CHARS: usize = 2000;

/// The facts a proposal may be based on. Deliberately flat strings: this is
/// a prompt input, not a place to thread live objects through, and anything
/// richer invites passing the whole `MonitorRow` — spec, credential refs and
/// all — into a model's context.
#[derive(Debug, Clone, Default)]
pub struct Context {
    /// `github_actions`, `mur_run`, … — the adapter, not the URL.
    pub source_type: String,
    /// The terminal outcome, as the monitor recorded it.
    pub outcome: String,
    /// The source's own error text, if it gave one.
    pub error: Option<String>,
    /// What was already tried this cycle and how it ended, e.g.
    /// `"rerun: failed"`. Without this the model re-proposes what just
    /// failed, which the remediation cap would then spend.
    pub attempted: Vec<String>,
    /// Tail of whatever logs were collected. Truncated, then redacted.
    pub log_tail: Option<String>,
}

/// Redact, then truncate — in that order.
///
/// The reverse loses: truncating first can cut a secret in half, leaving a
/// fragment that no longer matches the redactor's pattern but is still a
/// fragment of a secret. The action store's `store_result` orders it the
/// same way for the same reason.
fn clean(s: &str, max: usize) -> String {
    let red = redact_home_path(&redact_secrets(s)).into_owned();
    if red.chars().count() <= max {
        return red;
    }
    let kept: String = red.chars().skip(red.chars().count() - max).collect();
    format!("…{kept}")
}

/// The instruction half. Separate from the facts so it is obvious that the
/// verb set the model is told about is the same constant the parser enforces
/// — a prompt that offered a fourth verb would produce proposals this
/// process can only refuse.
pub fn system_prompt() -> String {
    format!(
        "You advise a monitoring daemon about one failed job. Reply with ONE JSON object and \
nothing else:\n\
{{\"action_type\": \"<verb>\", \"params\": {{}}, \"reason\": \"<one sentence>\"}}\n\n\
`action_type` MUST be exactly one of: {verbs}.\n\
- notify: tell the human, nothing more.\n\
- collect_logs: fetch more of the source's logs first.\n\
- rerun: re-run the failed jobs, for a failure you judge transient.\n\n\
Do not name any other verb; a verb outside that list voids your whole reply. \
Do not state a risk level or a tier — you are not consulted about those. \
If nothing is warranted, choose notify and say why in `reason`.",
        verbs = super::PROPOSABLE.join(", ")
    )
}

/// The facts half, redacted and bounded.
pub fn user_prompt(ctx: &Context) -> String {
    let mut out = format!(
        "source: {}\noutcome: {}\n",
        clean(&ctx.source_type, 64),
        clean(&ctx.outcome, 64)
    );
    if let Some(e) = &ctx.error {
        out.push_str(&format!("error: {}\n", clean(e, 400)));
    }
    if ctx.attempted.is_empty() {
        out.push_str("already tried: nothing — the spec listed no actions for this failure\n");
    } else {
        out.push_str("already tried: ");
        let tried: Vec<String> = ctx.attempted.iter().map(|a| clean(a, 80)).collect();
        out.push_str(&tried.join("; "));
        out.push('\n');
    }
    if let Some(l) = &ctx.log_tail {
        out.push_str(&format!("log tail:\n{}\n", clean(l, LOG_TAIL_MAX_CHARS)));
    }
    out
}

#[cfg(test)]
mod tests;
