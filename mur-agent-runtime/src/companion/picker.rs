//! Picker state types. Selection algorithm lands in M4.2.
//! Spec §3.4 / §4.5.

use chrono::{DateTime, Utc};
use mur_common::companion::Situation;
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
