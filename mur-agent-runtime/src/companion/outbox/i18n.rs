//! i18n step (step 10 of the outbox tick loop) — `handle_i18n` wraps
//! `crate::companion::i18n::ensure_locale` and interprets the result.

use chrono::{DateTime, Utc};
use rand::RngCore;

use crate::companion::i18n::{EnsureLocaleOutcome, ensure_locale};
use crate::companion::telemetry::OutboxEvent;

use super::{Outbox, backoff_for_attempt};

/// Internal result of the i18n step.
pub(super) enum I18nResult {
    /// Caller should use this body (original or translated).
    UseBody(String),
    /// Translation failed; send was paused until `resume_at`.
    Paused(DateTime<Utc>),
    /// Terminal: 4 failures exhausted; message dropped.
    Terminal,
}

impl<R: RngCore + Send> Outbox<R> {
    /// Call `i18n::ensure_locale` and interpret the result.
    ///
    /// `attempt_index` is the current attempt (0 = first try from the new-send
    /// path; ≥1 means we are in a resume cycle).
    ///
    /// On `QueuedRetry`: if `attempt_index` < 4 → schedule next backoff and
    /// return `I18nResult::Paused`; if attempt_index ≥ 4 → terminal drop.
    pub(super) async fn handle_i18n(
        &mut self,
        id: &str,
        body: &str,
        locale: &str,
        now_utc: DateTime<Utc>,
        attempt_index: u8,
    ) -> I18nResult {
        match ensure_locale(body, locale, self.llm.as_ref(), false).await {
            EnsureLocaleOutcome::Original => I18nResult::UseBody(body.to_string()),
            EnsureLocaleOutcome::Translated(new_body) => I18nResult::UseBody(new_body),
            // Unreachable in proactive path (reactive=false), but treat as Original.
            EnsureLocaleOutcome::OriginalWithLog(_err) => I18nResult::UseBody(body.to_string()),
            EnsureLocaleOutcome::QueuedRetry(_err) => {
                // append LocaleMismatchUnresolved event for this attempt.
                let _ = self.ledger.append(&OutboxEvent::LocaleMismatchUnresolved {
                    id: id.to_string(),
                    attempts: attempt_index + 1,
                    at: now_utc,
                });

                if let Some(backoff) = backoff_for_attempt(attempt_index) {
                    let resume_at = now_utc + backoff;
                    let _ = self.ledger.append(&OutboxEvent::MessagePaused {
                        id: id.to_string(),
                        resume_at,
                        reason: "locale_retry".to_string(),
                    });
                    I18nResult::Paused(resume_at)
                } else {
                    // 4th failure — terminal drop (attempts 0..3 exhausted).
                    let _ = self.ledger.append(&OutboxEvent::LocaleMismatchUnresolved {
                        id: id.to_string(),
                        attempts: 4,
                        at: now_utc,
                    });
                    let _ = self.ledger.append(&OutboxEvent::MessageDropped {
                        id: id.to_string(),
                        reason: "locale_unresolved".to_string(),
                    });
                    I18nResult::Terminal
                }
            }
        }
    }
}
