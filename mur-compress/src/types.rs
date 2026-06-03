use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentType {
    SearchResults,
    BuildLog,
    GitDiff,
    Json,
    Generic,
}

impl ContentType {
    pub fn as_str(self) -> &'static str {
        match self {
            ContentType::SearchResults => "search_results",
            ContentType::BuildLog => "build_log",
            ContentType::GitDiff => "git_diff",
            ContentType::Json => "json",
            ContentType::Generic => "generic",
        }
    }
}

pub struct CompressCtx<'a> {
    pub query: Option<&'a str>,
    pub config: &'a crate::config::CompressConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressOutput {
    pub compressed: String,
    pub hash: Option<String>,
    pub transforms: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum RetrieveResult {
    Full {
        content_type: String,
        original_content: String,
        item_count: usize,
    },
    Filtered {
        query: String,
        count: usize,
        results: Vec<String>,
    },
    NotFound,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressResult {
    pub compressed: String,
    pub hash: Option<String>,
    pub original_tokens: usize,
    pub compressed_tokens: usize,
    pub tokens_saved: usize,
    pub savings_percent: f32,
    pub transforms: Vec<String>,
    pub content_type: ContentType,
}

pub type CompressResultErr<T> = Result<T, CompressError>;

#[derive(Debug, Error)]
pub enum CompressError {
    #[error("tokenizer error: {0}")]
    Tokenizer(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("store error: {0}")]
    Store(String),
    #[error("parse error: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_type_as_str_roundtrips() {
        assert_eq!(ContentType::Json.as_str(), "json");
        assert_eq!(ContentType::SearchResults.as_str(), "search_results");
        assert_eq!(ContentType::BuildLog.as_str(), "build_log");
        assert_eq!(ContentType::GitDiff.as_str(), "git_diff");
        assert_eq!(ContentType::Generic.as_str(), "generic");
    }
}
