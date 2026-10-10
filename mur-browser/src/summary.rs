//! Derived, display-only views of a [`Run`]: domain, one-line summary,
//! search text and a frecency score. Nothing here is persisted — the YAML
//! stays exactly what the recorder wrote.

use chrono::{DateTime, Utc};
use unicode_width::UnicodeWidthChar;

use crate::recorder::{Action, Run};

/// Width (in display columns, not bytes) of the derived summary before `…`.
/// 40 keeps a listing row readable in an 80-column terminal next to the name
/// and domain columns; CJK glyphs count as two columns.
pub const SUMMARY_WIDTH: usize = 40;

/// Half-life for frecency decay. One week: a recording replayed daily
/// stays on top, one untouched for a month sinks below recent ones.
pub const FRECENCY_HALF_LIFE_DAYS: f64 = 7.0;

/// Separator between step intents in a derived summary.
const SUMMARY_JOINER: &str = " → ";
/// Appended when a summary is cut to [`SUMMARY_WIDTH`].
const ELLIPSIS: char = '…';
const SECONDS_PER_DAY: f64 = 86_400.0;

impl Run {
    /// Host of the first `goto` step, if it parses as a URL.
    pub fn domain(&self) -> Option<String> {
        let goto = self.steps.iter().find(|s| s.action == Action::Goto)?;
        let url = url::Url::parse(goto.value.as_deref()?).ok()?;
        url.host_str().map(str::to_owned)
    }

    /// One-line label for listings: the description when set, otherwise the
    /// non-`goto` intents joined with `→`; cut to [`SUMMARY_WIDTH`] columns.
    /// Display only — use [`Run::search_text`] for matching.
    pub fn summary(&self) -> String {
        let raw = match self.description.as_deref().map(str::trim) {
            Some(d) if !d.is_empty() => d.to_owned(),
            _ => self.derived_summary(),
        };
        truncate_to_width(&raw, SUMMARY_WIDTH)
    }

    fn derived_summary(&self) -> String {
        let mut intents: Vec<&str> = self
            .steps
            .iter()
            .filter(|s| s.action != Action::Goto)
            .map(|s| s.intent.as_str())
            .collect();
        if intents.is_empty() {
            // A goto-only run still deserves a label.
            intents = self.steps.iter().map(|s| s.intent.as_str()).collect();
        }
        intents.join(SUMMARY_JOINER)
    }

    /// Everything `--grep` and the picker match against, newline-joined and
    /// never truncated: name, description, tags, domain, every full intent.
    pub fn search_text(&self) -> String {
        let mut parts: Vec<String> = vec![self.name.clone()];
        parts.extend(self.description.clone());
        parts.extend(self.tags.iter().cloned());
        parts.extend(self.domain());
        parts.extend(self.steps.iter().map(|s| s.intent.clone()));
        parts.join("\n")
    }

    /// Sortable score, higher first. Replayed runs score `count · 2^(−age/half-life)`
    /// (always > 0); unreplayed runs score `−seconds since recorded_at` (≤ 0), so
    /// they sit below every replayed run and keep newest-first order among
    /// themselves. `now` is a parameter so tests need no clock.
    pub fn frecency(&self, now: DateTime<Utc>) -> f64 {
        frecency_score(self.recorded_at, self.replayed_at, self.replay_count, now)
    }
}

/// The frecency formula on its raw inputs, so a listing row that carries only
/// these fields scores exactly like the [`Run`] it came from.
pub fn frecency_score(
    recorded_at: DateTime<Utc>,
    replayed_at: Option<DateTime<Utc>>,
    replay_count: u32,
    now: DateTime<Utc>,
) -> f64 {
    if replay_count == 0 {
        return -seconds_between(recorded_at, now);
    }
    let last = replayed_at.unwrap_or(recorded_at);
    let age_days = seconds_between(last, now) / SECONDS_PER_DAY;
    let decay = 0.5_f64.powf(age_days / FRECENCY_HALF_LIFE_DAYS);
    (f64::from(replay_count) * decay).max(f64::MIN_POSITIVE)
}

/// Non-negative seconds from `earlier` to `now` (clock skew clamps to 0).
fn seconds_between(earlier: DateTime<Utc>, now: DateTime<Utc>) -> f64 {
    (now - earlier).num_milliseconds().max(0) as f64 / 1000.0
}

/// Cut `s` so it occupies at most `width` display columns, ending in `…`
/// when anything was dropped.
fn truncate_to_width(s: &str, width: usize) -> String {
    let col = |c: char| c.width().unwrap_or(0);
    if s.chars().map(col).sum::<usize>() <= width {
        return s.to_owned();
    }
    let budget = width.saturating_sub(col(ELLIPSIS));
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = col(c);
        if used + w > budget {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push(ELLIPSIS);
    out
}

#[cfg(test)]
mod tests;
