use crate::store::embedding::{EmbeddingConfig, EmbeddingProvider};
use mur_common::config::Config;

/// When user picks Anthropic as cloud LLM and Voyage as embedding,
/// embedding.openai_url must be None (Voyage uses its own canonical
/// URL, not OpenRouter's).
#[test]
fn cloud_embedding_does_not_inherit_llm_openai_url() {
    let mut cfg = Config::default();
    cfg.llm.provider = "openai".into();
    cfg.llm.openai_url = Some("https://openrouter.ai/api/v1".into());
    // simulate what select_cloud_embedding does for anthropic→voyage:
    cfg.embedding.provider = "anthropic".into();
    cfg.embedding.model = "voyage-3-lite".into();
    cfg.embedding.api_key_env = Some("ANTHROPIC_API_KEY".into());
    cfg.embedding.openai_url = None;

    // Round-trip through EmbeddingConfig — should resolve OpenAI variant
    // with default base_url (api.openai.com), NOT OpenRouter.
    let ec = EmbeddingConfig::from_config(&cfg);
    match ec.provider {
        EmbeddingProvider::OpenAI { base_url, .. } => {
            assert_eq!(base_url, "https://api.openai.com/v1");
        }
        _ => panic!("expected OpenAI variant"),
    }
}
