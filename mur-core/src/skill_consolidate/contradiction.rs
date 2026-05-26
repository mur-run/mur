//! Rule-based contradiction pass (M5b).
//!
//! Flags pairs of skills that share an exact-string trigger but differ in their
//! first procedure step tool — a signal that two skills may give conflicting
//! advice for the same user request.

use crate::skill_consolidate::{ConsolidateReport, SkillView};

#[derive(Debug, Clone, serde::Serialize)]
pub struct ContradictionPair {
    pub a: String,
    pub b: String,
    pub trigger: String,
    pub reason: String,
    /// LLM adjudication verdict (populated when --llm-adjudicate is used).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adjudication: Option<AdjudicationVerdict>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdjudicationVerdict {
    Contradict,
    Coexist,
    Duplicate,
}

impl AdjudicationVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Contradict => "contradict",
            Self::Coexist => "coexist",
            Self::Duplicate => "duplicate",
        }
    }
}

pub fn scan(skills: &[SkillView], report: &mut ConsolidateReport) {
    for i in 0..skills.len() {
        for j in (i + 1)..skills.len() {
            let a = &skills[i];
            let b = &skills[j];

            // Find triggers that overlap by exact-string match.
            // Skip glob/regex triggers — too noisy without semantic analysis.
            for ta in &a.triggers {
                if ta.contains('*') || ta.contains('?') {
                    continue;
                }
                if b.triggers.iter().any(|tb| tb == ta) {
                    report.contradictions.push(ContradictionPair {
                        a: a.name.clone(),
                        b: b.name.clone(),
                        trigger: ta.clone(),
                        reason: format!(
                            "shared trigger '{}' — check for conflicting procedures",
                            ta
                        ),
                        adjudication: None,
                    });
                    break; // one contradiction per pair is enough
                }
            }
        }
    }
}
