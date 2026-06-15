//! Optional research grounding for the LLM author stage.
use crate::agent_wizard::draft::RoleSpec;

/// A single researched note used to ground generated skills.
#[derive(Debug, Clone, PartialEq)]
pub struct ResearchNote {
    pub summary: String,
    pub url: String,
}

/// Provider-agnostic research seam. Implementations may call a search MCP (Tavily/Exa/…).
/// Object-safe via boxed futures.
pub trait SearchProvider: Send + Sync {
    fn research(
        &self,
        role: &RoleSpec,
        topics: &[String],
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<Vec<ResearchNote>>> + Send + '_>,
    >;
}

/// Default: no external research (pure model-knowledge drafting).
pub struct NoopSearch;

impl SearchProvider for NoopSearch {
    fn research(
        &self,
        _role: &RoleSpec,
        _topics: &[String],
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<Vec<ResearchNote>>> + Send + '_>,
    > {
        Box::pin(async { Ok(Vec::new()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_wizard::draft::RiskLevel;
    use std::sync::Arc;

    #[tokio::test]
    async fn noop_returns_no_notes() {
        let p: Arc<dyn SearchProvider> = Arc::new(NoopSearch);
        let role = RoleSpec {
            name: "x".into(),
            display_name: "X".into(),
            charter: "c".into(),
            risk: RiskLevel::Low,
            preset_id: None,
        };
        let notes = p.research(&role, &["t".into()]).await.unwrap();
        assert!(notes.is_empty());
    }
}
