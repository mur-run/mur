//! Optional research grounding for the LLM author stage.

/// A single researched note used to ground generated skills.
#[derive(Debug, Clone, PartialEq)]
pub struct ResearchNote {
    pub summary: String,
    pub url: String,
}
