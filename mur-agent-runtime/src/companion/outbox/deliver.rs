//! Delivery step (steps 11 + 12 of the outbox tick loop) — push the message
//! through the notifier, finalise rhythm state, and append `MessageSent`.

use chrono::{DateTime, Local, Utc};
use mur_common::companion::{Signal, Situation};
use rand::RngCore;

use crate::companion::notifier::{CompanionMessage, NotifyOutcome};
use crate::companion::picker::TemplateId;
use crate::companion::telemetry::OutboxEvent;

use super::{Outbox, SkipReason, TickOutcome};

impl<R: RngCore + Send> Outbox<R> {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn deliver_and_finalise(
        &mut self,
        id: &str,
        body: &str,
        locale: &str,
        situation: Situation,
        template_id: TemplateId,
        now_utc: DateTime<Utc>,
        now_local: DateTime<Local>,
    ) -> TickOutcome {
        let msg = CompanionMessage {
            id: id.to_string(),
            situation: situation.clone(),
            template_id: template_id.clone(),
            locale: locale.to_string(),
            body: body.to_string(),
            generated_at: now_utc,
        };

        match self.notifier.send(&msg).await {
            Ok(NotifyOutcome::Delivered) => {
                // continue to step 12
            }
            Ok(NotifyOutcome::Skipped { reason }) => {
                let _ = self.ledger.append(&OutboxEvent::MessageDropped {
                    id: id.to_string(),
                    reason: format!("notifier_skipped:{reason}"),
                });
                return TickOutcome::Skipped {
                    reason: SkipReason::NotifierSkipped,
                };
            }
            Ok(NotifyOutcome::Failed(_)) | Err(_) => {
                let _ = self.ledger.append(&OutboxEvent::MessageDropped {
                    id: id.to_string(),
                    reason: "notifier_failed".to_string(),
                });
                return TickOutcome::Skipped {
                    reason: SkipReason::NotifierFailed,
                };
            }
        }

        // ── Step 12: finalise ────────────────────────────────────────────────
        let _ = self.ledger.append(&OutboxEvent::MessageSent {
            id: id.to_string(),
            channel: self.notifier.name().to_string(),
            sent_at: now_utc,
        });

        let today = now_local.date_naive();
        self.picker.record(&template_id, Signal::Sent, now_utc);
        self.last_send_at = Some(now_local);
        self.sent_today += 1;
        if situation == Situation::MorningGreeting {
            self.morning_sent_today = Some(today);
        }

        TickOutcome::Sent {
            id: id.to_string(),
            situation,
            template_id,
        }
    }
}
