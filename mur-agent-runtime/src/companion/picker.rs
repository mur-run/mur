//! Picker state + selection algorithm.
//! Spec §3.4 / §4.5.

use chrono::{DateTime, Utc};
use mur_common::companion::{Signal, Situation};
use rand::SeedableRng;
use rand::distributions::{Distribution, WeightedIndex};
use rand::rngs::StdRng;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type TemplateId = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TemplateState {
    pub id: TemplateId,
    pub situation: Situation,
    pub weight: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub pos_count: u32,
    #[serde(default)]
    pub neg_count: u32,
    #[serde(default)]
    pub dismiss_count: u32,
    pub cooldown_days: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BanditState {
    #[serde(default = "current_version")]
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub morning_sent_today: Option<chrono::NaiveDate>,
    #[serde(default)]
    pub templates: BTreeMap<TemplateId, TemplateState>,
}

fn current_version() -> u32 {
    1
}

/// Picker = bandit-state + RNG. Selection is `WeightedIndex` over eligible
/// (cooldown-elapsed) templates for the requested situation.
pub struct Picker<R: rand::RngCore + Send = StdRng> {
    pub state: BanditState,
    pub rng: R,
}

impl Picker<StdRng> {
    /// Production picker — seeded from entropy.
    pub fn from_state(state: BanditState) -> Self {
        Self {
            state,
            rng: StdRng::from_entropy(),
        }
    }

    /// Test picker — deterministic seed.
    pub fn with_seed(state: BanditState, seed: u64) -> Self {
        Self {
            state,
            rng: StdRng::seed_from_u64(seed),
        }
    }
}

impl<R: rand::RngCore + Send> Picker<R> {
    /// Pick a `template_id` for the given situation, or `None` if all eligible
    /// templates are on cooldown / pool empty.
    pub fn pick(&mut self, situation: Situation, now: DateTime<Utc>) -> Option<TemplateId> {
        let eligible: Vec<&TemplateState> = self
            .state
            .templates
            .values()
            .filter(|t| t.situation == situation)
            .filter(|t| match t.last_used_at {
                Some(last) => (now - last).num_days() >= t.cooldown_days as i64,
                None => true,
            })
            .collect();
        if eligible.is_empty() {
            return None;
        }

        let weights: Vec<f32> = eligible.iter().map(|t| t.weight).collect();
        let dist = WeightedIndex::new(&weights).ok()?;
        let idx = dist.sample(&mut self.rng);
        Some(eligible[idx].id.clone())
    }

    /// Record a signal against a template, updating its weight + counts.
    pub fn record(&mut self, id: &TemplateId, signal: Signal, now: DateTime<Utc>) {
        let Some(t) = self.state.templates.get_mut(id) else {
            tracing::warn!("picker.record: unknown template {id}");
            return;
        };
        match signal {
            Signal::Positive => {
                t.weight = (t.weight * 1.2).min(5.0);
                t.pos_count += 1;
            }
            Signal::Negative => {
                t.weight = (t.weight * 0.5).max(0.1);
                t.neg_count += 1;
            }
            Signal::Dismiss => {
                t.dismiss_count += 1;
            }
            Signal::Sent => {
                t.last_used_at = Some(now);
            }
        }
    }
}
