//! Static prefix-matched preference tables for picking the "best" model
//! to recommend when multiple are available locally. Future-proof against
//! new tags via prefix matching.

/// Ordered preference for embedding models, descending by score.
/// Both Ollama tag form (`name:size`) and HuggingFace id form
/// (`mlx-community/Foo`) appear at equal score.
pub const EMBEDDING_PREFERENCE: &[(&str, u32)] = &[
    ("qwen3.5-embedding",       105),  // future-proof
    ("Qwen3-Embedding-8B",      100),
    ("qwen3-embedding:8b",      100),
    ("Qwen3-Embedding-4B",       90),
    ("qwen3-embedding:4b",       90),
    ("bge-m3",                   80),
    ("jina-embeddings-v3",       75),
    ("Qwen3-Embedding-0.6B",     70),
    ("qwen3-embedding:0.6b",     70),
    ("embeddinggemma",           55),
    ("nomic-embed-text",         40),
    ("all-minilm",               20),
];

/// Ordered preference for chat / completion LLMs (Mode 3 only).
/// Aligned with the curated picks in `cmd/init_local.rs::OLLAMA_RECS` /
/// `MLX_RECS`. Multilingual-first ordering.
pub const LLM_PREFERENCE: &[(&str, u32)] = &[
    ("Qwen3.5-9B",                95),
    ("qwen3.5:9b",                95),
    ("Qwen3.5-4B",                90),
    ("qwen3.5:4b",                90),
    ("Gemma4-E2B",                85),
    ("gemma4:e2b",                85),
    ("Qwen3-9B",                  70),
    ("qwen3:9b",                  70),
    ("Qwen3-4B",                  65),
    ("qwen3:4b",                  65),
    ("llama3.3",                  60),
];

/// Highest-scoring prefix that is a substring of `id`. Returns 0 when no
/// prefix matches. Case-sensitive — both Ollama (`qwen3-embedding:0.6b`)
/// and HF (`Qwen3-Embedding-0.6B`) forms must appear in the table.
pub fn rank(id: &str, table: &[(&str, u32)]) -> u32 {
    table
        .iter()
        .filter(|(prefix, _)| id.contains(prefix))
        .map(|(_, score)| *score)
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_tag_form_ranked() {
        assert_eq!(rank("qwen3-embedding:0.6b", EMBEDDING_PREFERENCE), 70);
        assert_eq!(rank("qwen3-embedding:8b", EMBEDDING_PREFERENCE), 100);
        assert_eq!(rank("bge-m3", EMBEDDING_PREFERENCE), 80);
    }

    #[test]
    fn hf_id_form_ranked() {
        assert_eq!(
            rank("mlx-community/Qwen3-Embedding-0.6B-8bit", EMBEDDING_PREFERENCE),
            70
        );
        assert_eq!(
            rank("mlx-community/Qwen3-Embedding-8B-4bit-DWQ", EMBEDDING_PREFERENCE),
            100
        );
    }

    #[test]
    fn unknown_id_returns_zero() {
        assert_eq!(rank("randomuser/foo-base", EMBEDDING_PREFERENCE), 0);
        assert_eq!(rank("", EMBEDDING_PREFERENCE), 0);
    }

    #[test]
    fn future_qwen35_embedding_wins() {
        // When Alibaba ships qwen3.5-embedding, the prefix table should pick
        // it over current SOTA without any code change.
        assert_eq!(
            rank("qwen3.5-embedding:0.6b", EMBEDDING_PREFERENCE),
            105
        );
        assert!(
            rank("qwen3.5-embedding:0.6b", EMBEDDING_PREFERENCE)
                > rank("qwen3-embedding:8b", EMBEDDING_PREFERENCE)
        );
    }

    #[test]
    fn llm_table_separate() {
        assert_eq!(rank("qwen3.5:9b", LLM_PREFERENCE), 95);
        assert_eq!(rank("qwen3.5:9b", EMBEDDING_PREFERENCE), 0); // not in embedding table
    }

    #[test]
    fn case_sensitive_distinguishes_forms() {
        // The two forms are distinct entries at the same score; rank is the
        // max of all matching prefixes, so a string matching either is fine.
        assert_eq!(rank("Qwen3-Embedding-8B", EMBEDDING_PREFERENCE), 100);
        assert_eq!(rank("qwen3-embedding:8b", EMBEDDING_PREFERENCE), 100);
    }
}
