use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Global MUR configuration (~/.mur/config.yaml)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct Config {
    #[serde(default)]
    pub embedding: EmbeddingConfig,

    #[serde(default)]
    pub llm: LlmConfig,

    #[serde(default)]
    pub retrieval: RetrievalConfig,

    #[serde(default)]
    pub paths: PathConfig,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingConfig {
    /// "ollama" or "openai"
    #[serde(default = "default_embedding_provider")]
    pub provider: String,

    /// Model name (e.g. "nomic-embed-text", "text-embedding-3-small")
    #[serde(default = "default_embedding_model")]
    pub model: String,

    /// Vector dimensions (fixed after first index build)
    #[serde(default = "default_dimensions")]
    pub dimensions: usize,

    /// Ollama endpoint
    #[serde(default = "default_ollama_endpoint")]
    pub ollama_endpoint: String,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: default_embedding_provider(),
            model: default_embedding_model(),
            dimensions: default_dimensions(),
            ollama_endpoint: default_ollama_endpoint(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// "anthropic", "openai", or "ollama"
    #[serde(default = "default_llm_provider")]
    pub provider: String,

    #[serde(default = "default_llm_model")]
    pub model: String,

    /// API key env var name (e.g. "ANTHROPIC_API_KEY")
    #[serde(default)]
    pub api_key_env: Option<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: default_llm_provider(),
            model: default_llm_model(),
            api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalConfig {
    /// Max patterns to inject per query
    #[serde(default = "default_max_patterns")]
    pub max_patterns: usize,

    /// Max tokens for injected content
    #[serde(default = "default_max_tokens")]
    pub max_tokens: usize,

    /// Minimum score threshold
    #[serde(default = "default_min_score")]
    pub min_score: f64,

    /// MMR diversity threshold (cosine > this = too similar)
    #[serde(default = "default_mmr_threshold")]
    pub mmr_threshold: f64,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            max_patterns: default_max_patterns(),
            max_tokens: default_max_tokens(),
            min_score: default_min_score(),
            mmr_threshold: default_mmr_threshold(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathConfig {
    /// Root MUR directory (default: ~/.mur)
    #[serde(default = "default_mur_dir")]
    pub mur_dir: PathBuf,
}

impl Default for PathConfig {
    fn default() -> Self {
        Self {
            mur_dir: default_mur_dir(),
        }
    }
}

fn default_embedding_provider() -> String { "ollama".to_string() }
fn default_embedding_model() -> String { "nomic-embed-text".to_string() }
fn default_dimensions() -> usize { 768 }
fn default_ollama_endpoint() -> String { "http://localhost:11434".to_string() }
fn default_llm_provider() -> String { "anthropic".to_string() }
fn default_llm_model() -> String { "claude-sonnet-4-20250514".to_string() }
fn default_max_patterns() -> usize { 5 }
fn default_max_tokens() -> usize { 2000 }
fn default_min_score() -> f64 { 0.35 }
fn default_mmr_threshold() -> f64 { 0.85 }
fn default_mur_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mur")
}
