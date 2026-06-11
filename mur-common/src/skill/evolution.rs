//! Skill evolution log — records every generation of a skill.

use serde::{Deserialize, Serialize};

pub const CURRENT_GENERATION: u32 = 0;

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EvolutionEvent {
    pub version: String,
    #[serde(default)]
    pub generation: u32,
    pub source: String,
    pub changes: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_score: Option<f64>,
    #[serde(default)]
    pub timestamp: String,
}

impl EvolutionEvent {
    pub fn initial_human(publisher: &str, version: &str) -> Self {
        Self {
            version: version.to_string(),
            generation: 0,
            source: format!("human:{publisher}"),
            changes: "Initial creation".into(),
            quality_score: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }

    pub fn evolved(version: &str, generation: u32, changes: &str, score: f64) -> Self {
        Self {
            version: version.to_string(),
            generation,
            source: "agent:evolver".into(),
            changes: changes.into(),
            quality_score: Some(score),
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Constructor for M7b recombination events. `parent_a` and `parent_b`
    /// are stringified `SkillRef`s (e.g. `local/foo` or `agent://bob/bar`).
    pub fn recombined(
        version: &str,
        generation: u32,
        parent_a: &str,
        parent_b: &str,
        strategy: &str,
        output_skill: &str,
    ) -> Self {
        Self {
            version: version.to_string(),
            generation,
            source: "agent:recombiner".into(),
            changes: format!(
                "recombine: a={parent_a}, b={parent_b}, strategy={strategy}, output={output_skill}"
            ),
            quality_score: None,
            timestamp: chrono::Utc::now().to_rfc3339(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_human_produces_correct_shape() {
        let event = EvolutionEvent::initial_human("david", "0.1.0");
        assert_eq!(event.version, "0.1.0");
        assert_eq!(event.generation, 0);
        assert_eq!(event.source, "human:david");
        assert_eq!(event.changes, "Initial creation");
        assert!(event.quality_score.is_none());
        assert!(!event.timestamp.is_empty());
    }

    #[test]
    fn evolved_bumps_generation_and_sets_quality_score() {
        let event = EvolutionEvent::evolved("0.1.1", 1, "fixed tool name", 0.85);
        assert_eq!(event.version, "0.1.1");
        assert_eq!(event.generation, 1);
        assert_eq!(event.source, "agent:evolver");
        assert_eq!(event.changes, "fixed tool name");
        assert_eq!(event.quality_score, Some(0.85));
    }

    #[test]
    fn recombined_sets_recombiner_source_and_packs_metadata() {
        let event = EvolutionEvent::recombined(
            "0.1.0",
            5,
            "local/research-prices",
            "agent://bob/lookup",
            "union",
            "combined-research",
        );
        assert_eq!(event.source, "agent:recombiner");
        assert_eq!(event.version, "0.1.0");
        assert_eq!(event.generation, 5);
        assert!(event.changes.starts_with("recombine: "));
        assert!(event.changes.contains("a=local/research-prices"));
        assert!(event.changes.contains("b=agent://bob/lookup"));
        assert!(event.changes.contains("strategy=union"));
        assert!(event.changes.contains("output=combined-research"));
        assert!(event.quality_score.is_none());
        assert!(!event.timestamp.is_empty());
    }

    #[test]
    fn evolution_event_roundtrips() {
        let event = EvolutionEvent::evolved("1.2.3", 3, "patched X", 0.92);
        let json = serde_json::to_string(&event).unwrap();
        let back: EvolutionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, event.version);
        assert_eq!(back.generation, event.generation);
        assert_eq!(back.source, event.source);
        assert_eq!(back.changes, event.changes);
        assert_eq!(back.quality_score, event.quality_score);
    }
}
