//! Generation step (steps 8 + 9 of the outbox tick loop) — `generate_with_lint`
//! with one regenerate, plus prompt-seed placeholder substitution.

use chrono::{DateTime, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::companion::linter;
use crate::companion::picker::TemplateId;
use crate::companion::telemetry::OutboxEvent;
use crate::llm::{LlmError, LlmRequest, RichMessage};

use super::Outbox;

/// Internal result of the generate+lint loop.
pub(super) enum GenerateResult {
    Ok(String),
    RateLimit,
    LinterPersistent,
}

impl<R: RngCore + Send> Outbox<R> {
    /// Substitute companion placeholders into a `prompt_seed` string.
    ///
    /// Reuses [`crate::companion::voice::apply_placeholders`] so the
    /// system-prompt path (voice template) and the user-prompt path
    /// (this method, M2.2.4) never diverge in their substitution rules:
    /// template-defined tokens (`{{LOCALE}}`, `{{FIRST_MEMORY}}`,
    /// `{{FIRST_MEMORY_PARAGRAPH}}`) expand first, user-supplied tokens
    /// (`{{NAME_FOR_USER}}`, `{{FORMALITY}}`, `{{EXTRA_INSTRUCTIONS}}`)
    /// expand last (so they can't inject re-substituted `{{...}}` tokens).
    fn substitute_prompt_seed(&self, seed: &str) -> String {
        use crate::companion::voice::{VoiceInput, apply_placeholders};
        let input = VoiceInput {
            relationship: self.relationship.clone(),
            locale: &self.locale,
            name_for_user: &self.name_for_user,
            first_memory: self.first_memory.as_deref(),
            formality: &self.formality,
            extra_instructions: &self.extra_instructions,
        };
        apply_placeholders(seed, &self.locale, &input)
    }

    /// Attempt to generate a lint-passing body.
    ///
    /// On first lint failure, appends `MessageGenerated { regen_count: 0 }` and
    /// retries **once** with a `"\n[regenerate]"` suffix on the user prompt so
    /// the `StubLlm` can match a distinct scenario.  On second failure, appends
    /// both a second `MessageGenerated` and `MessageDropped { linter_persistent }`.
    ///
    /// **M2.2.4** — when the picked `template_id` has a non-empty
    /// `prompt_seed` registered in `self.prompt_seeds`, that seed (with
    /// placeholder substitution) is used as the user prompt instead of the
    /// legacy `"Compose one short message…"` line. Empty / missing seeds
    /// fall back to the legacy prompt so existing behaviour doesn't regress.
    pub(super) async fn generate_with_lint(
        &mut self,
        id: &str,
        template_id: &TemplateId,
        situation_str: &str,
        locale: &str,
        _now_utc: DateTime<Utc>,
    ) -> GenerateResult {
        // Resolve the picked template's prompt_seed (if any) and substitute
        // placeholders ONCE. Both regen attempts share the same base prompt;
        // only the trailing `[regenerate]` marker differs.
        let base_prompt: String = match self.prompt_seeds.get(template_id) {
            Some(seed) if !seed.trim().is_empty() => self.substitute_prompt_seed(seed),
            _ => format!(
                "Compose one short message for situation: {situation_str}, locale: {locale}"
            ),
        };

        for regen_count in 0u32..=1 {
            // Append "[regenerate]" on the retry so StubLlm can match a
            // distinct scenario (documented contract).
            let user_prompt = if regen_count == 0 {
                base_prompt.clone()
            } else {
                format!("{base_prompt}\n[regenerate]")
            };

            let req = LlmRequest {
                messages: vec![
                    RichMessage::Text {
                        role: "system".to_string(),
                        content: self.voice_md.clone(),
                    },
                    RichMessage::Text {
                        role: "user".to_string(),
                        content: user_prompt,
                    },
                ],
                temperature: None,
                max_tokens: None,
                tools: vec![],
                ..Default::default()
            };

            let text = match self.llm.generate(req).await {
                Ok(resp) => resp.text,
                Err(LlmError::RateLimit) => return GenerateResult::RateLimit,
                Err(e) => {
                    tracing::warn!("outbox: LLM error on attempt {regen_count}: {e}");
                    // Treat other errors like a lint failure — drop after second.
                    if regen_count == 1 {
                        let _ = self.ledger.append(&OutboxEvent::MessageDropped {
                            id: id.to_string(),
                            reason: "linter_persistent".to_string(),
                        });
                        return GenerateResult::LinterPersistent;
                    }
                    continue;
                }
            };

            let report = linter::check(&text, locale);
            let body_sha256 = hex::encode(Sha256::digest(text.as_bytes()));

            let _ = self.ledger.append(&OutboxEvent::MessageGenerated {
                id: id.to_string(),
                locale_used: locale.to_string(),
                body_sha256,
                linter_violations: report.violations.len() as u32,
                regen_count,
            });

            if report.passed {
                return GenerateResult::Ok(text);
            }

            // Lint failed.
            if regen_count == 1 {
                // Second failure — drop.
                let _ = self.ledger.append(&OutboxEvent::MessageDropped {
                    id: id.to_string(),
                    reason: "linter_persistent".to_string(),
                });
                return GenerateResult::LinterPersistent;
            }
            // regen_count == 0 → loop continues with regen_count = 1
        }

        // Unreachable, but satisfies the compiler.
        GenerateResult::LinterPersistent
    }
}
