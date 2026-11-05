//! The approval menu: the decisions a gate offers and how each row is worded.
//! Drawn by `approval` (the docked panel); acted on by `events`/`hitl`.

/// Hard-wrap `s` into rows at most `w` display columns wide.
///
/// Character-based rather than word-based: the payload is pretty-printed JSON
/// and shell, where breaking mid-token is honest and dropping the tail is not.
/// Widths come from `unicode_width` so a CJK argument does not overflow the
/// border.
pub(super) fn wrap_row(s: &str, w: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    if w == 0 || s.is_empty() {
        return vec![s.to_string()];
    }
    let mut rows = Vec::new();
    let mut cur = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw > w && !cur.is_empty() {
            rows.push(std::mem::take(&mut cur));
            used = 0;
        }
        cur.push(ch);
        used += cw;
    }
    rows.push(cur);
    rows
}

/// The decisions the approval modal offers, in menu order. The index IS the
/// contract between the renderer (which numbers them) and the key handler
/// (which acts on them), so they are declared once, here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HitlChoice {
    /// Approve this one call.
    Once,
    /// Approve, and stop asking for the SCOPE of this call for the rest of the
    /// session — a destination proved read-only (`ssh:user@host:ro`,
    /// `local:ro`) when this is a classifiable `bash` command, else the tool
    /// name.
    Tool,
    /// Approve, and stop asking for every tool for the rest of the session.
    All,
    /// Deny. Anything typed in the composer rides along as the follow-up.
    Deny,
}

/// Menu order. `Deny` is last so the destructive-by-default answer is never
/// the one under a reflex `Enter` (the selection starts at `Once`).
pub(crate) const HITL_CHOICES: [HitlChoice; 4] = [
    HitlChoice::Once,
    HitlChoice::Tool,
    HitlChoice::All,
    HitlChoice::Deny,
];

/// P3b-§10: a review member's gate always asks, so it offers no session
/// grant. `Deny` stays last for the same reflex-Enter reason.
pub(crate) const REVIEW_HITL_CHOICES: [HitlChoice; 2] = [HitlChoice::Once, HitlChoice::Deny];

/// The menu for the gate in the slot. The key handler and the renderer both
/// read it here, so a row the operator can see is a row a key can reach.
pub(crate) fn hitl_choices(
    origin: crate::cmd::fleet::review::murmur::bridge::HitlOrigin,
) -> &'static [HitlChoice] {
    match origin {
        crate::cmd::fleet::review::murmur::bridge::HitlOrigin::Own => &HITL_CHOICES,
        crate::cmd::fleet::review::murmur::bridge::HitlOrigin::Review => &REVIEW_HITL_CHOICES,
    }
}

impl HitlChoice {
    /// The row label. `grant` is what a session grant would actually cover for
    /// the call being gated — the row prints that rather than a fixed promise,
    /// because the fixed promise was sometimes false.
    pub(crate) fn label(self, grant: &crate::cmd::agent::cli::dest::Grant) -> String {
        match self {
            Self::Once => "Yes".to_string(),
            Self::Tool => grant.label(),
            // NOT "any tool": `stream_handler` ANDs `auto_approve` with
            // `tier_may_be_granted`, so this row can never cover `sudo`,
            // `rm -rf`, a `curl` upload or a `git push`. It used to promise
            // "any tool" anyway, which is how an operator could press it,
            // watch the same command ask again, and conclude the menu was
            // broken — the row was the only place that could have told them.
            Self::All => "Yes, and stop asking this session (risky calls still ask)".to_string(),
            Self::Deny => "No, and tell MUR what to do differently (Esc)".to_string(),
        }
    }
}
