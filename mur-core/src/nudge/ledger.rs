use crate::nudge::candidate::WorkflowCandidate;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum NudgeState {
    Surfaced,
    Accepted,
    Dismissed,
    Snoozed { until: String }, // RFC3339
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NudgeRecord {
    pub state: NudgeState,
    pub last_ts: String,
    pub surface_count: u32,
    /// Snapshot kept so accept can rebuild the draft without re-mining.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<WorkflowCandidate>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NudgeLedger {
    #[serde(default)]
    pub records: BTreeMap<String, NudgeRecord>,
}

impl NudgeLedger {
    pub fn default_path() -> PathBuf {
        crate::store::yaml::default_mur_dir().join("nudges.json")
    }
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(serde_json::from_str(&s).unwrap_or_default()),
            Err(_) => Ok(Self::default()),
        }
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
    pub fn get(&self, id: &str) -> Option<&NudgeRecord> {
        self.records.get(id)
    }
    pub fn set_state(&mut self, id: &str, state: NudgeState, now: DateTime<Utc>) {
        let rec = self.records.entry(id.to_string()).or_insert(NudgeRecord {
            state: NudgeState::Surfaced,
            last_ts: now.to_rfc3339(),
            surface_count: 0,
            candidate: None,
        });
        rec.state = state;
        rec.last_ts = now.to_rfc3339();
    }
    /// Mark a candidate Surfaced (storing its snapshot) and bump surface_count.
    pub fn mark_surfaced(&mut self, c: &WorkflowCandidate, now: DateTime<Utc>) {
        let rec = self.records.entry(c.id.clone()).or_insert(NudgeRecord {
            state: NudgeState::Surfaced,
            last_ts: now.to_rfc3339(),
            surface_count: 0,
            candidate: None,
        });
        rec.state = NudgeState::Surfaced;
        rec.last_ts = now.to_rfc3339();
        rec.surface_count += 1;
        rec.candidate = Some(c.clone());
    }

    /// Candidates eligible to surface: not accepted/dismissed, not currently
    /// snoozed, capped at `daily_cap` newly-actionable items.
    pub fn filter_actionable(
        &self,
        candidates: &[WorkflowCandidate],
        now: DateTime<Utc>,
        daily_cap: u32,
    ) -> Vec<WorkflowCandidate> {
        let mut out = Vec::new();
        for c in candidates {
            match self.records.get(&c.id).map(|r| &r.state) {
                Some(NudgeState::Accepted) | Some(NudgeState::Dismissed) => continue,
                Some(NudgeState::Snoozed { until }) => {
                    let expired = DateTime::parse_from_rfc3339(until)
                        .map(|u| now >= u.with_timezone(&Utc))
                        .unwrap_or(true);
                    if !expired {
                        continue;
                    }
                }
                _ => {}
            }
            out.push(c.clone());
            if out.len() as u32 >= daily_cap {
                break;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nudge::candidate::WorkflowCandidate;
    use chrono::{Duration, Utc};

    fn cand(id: &str) -> WorkflowCandidate {
        WorkflowCandidate {
            id: id.into(),
            title: "t".into(),
            suggested_name: "n".into(),
            steps_preview: vec![],
            session_count: 3,
            evidence_session_ids: vec![],
        }
    }

    #[test]
    fn dismissed_never_resurfaces() {
        let mut l = NudgeLedger::default();
        l.set_state("a", NudgeState::Dismissed, Utc::now());
        let actionable = l.filter_actionable(&[cand("a"), cand("b")], Utc::now(), 10);
        let ids: Vec<_> = actionable.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["b"]); // "a" dismissed -> excluded
    }

    #[test]
    fn snooze_hides_until_expiry() {
        let mut l = NudgeLedger::default();
        let now = Utc::now();
        l.set_state(
            "a",
            NudgeState::Snoozed {
                until: (now + Duration::days(3)).to_rfc3339(),
            },
            now,
        );
        assert!(l.filter_actionable(&[cand("a")], now, 10).is_empty());
        let later = now + Duration::days(4);
        assert_eq!(l.filter_actionable(&[cand("a")], later, 10).len(), 1);
    }

    #[test]
    fn daily_cap_limits_new_surfaces() {
        let l = NudgeLedger::default();
        let now = Utc::now();
        let out = l.filter_actionable(&[cand("a"), cand("b"), cand("c")], now, 2);
        assert_eq!(out.len(), 2); // cap = 2
    }
}
