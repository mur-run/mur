pub mod gemini;
pub mod mock;

use async_trait::async_trait;
use image::DynamicImage;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use thiserror::Error;

/// Image dimensions passed to the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageSize {
    pub width: u32,
    pub height: u32,
}

impl ImageSize {
    /// Parse from the preset's "WxH" string (e.g. "512x512").
    pub fn from_size_str(s: &str) -> Self {
        let mut parts = s.splitn(2, 'x').filter_map(|p| p.parse::<u32>().ok());
        let width = parts.next().unwrap_or(512);
        let height = parts.next().unwrap_or(512);
        Self { width, height }
    }
}

/// Cooperative cancellation handle shared between the caller and provider.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Errors returned by image generation providers.
#[derive(Debug, Error)]
pub enum ImageGenError {
    #[error("cancelled")]
    Cancelled,
    #[error("API error: {0}")]
    ApiError(String),
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Progress snapshot emitted after each image completes or fails.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RenderProgress {
    pub total: u32,
    pub done: u32,
    pub failed: u32,
    /// Expression ID currently being rendered, if any.
    pub current: Option<String>,
}

/// An (expression_id, full_prompt, negative_prompt) triple ready for the provider.
#[derive(Debug, Clone)]
pub struct ExpressionPrompt {
    pub id: String,
    pub prompt: String,
    pub negative_prompt: String,
}

/// Trait that all image generation backends implement.
#[async_trait]
pub trait ImageGenProvider: Send + Sync {
    /// Generate a single image and return it as a `DynamicImage`.
    async fn generate(
        &self,
        prompt: &str,
        negative: Option<&str>,
        size: ImageSize,
        source_image: Option<&Path>,
        cancel: CancelToken,
    ) -> Result<DynamicImage, ImageGenError>;

    /// Human-readable backend name (e.g. "mock", "gemini").
    fn name(&self) -> &str;

    /// Estimated USD cost per generated image, for the settings UI.
    fn estimated_cost_per_image_usd(&self) -> f64 {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_size_parses_standard() {
        let sz = ImageSize::from_size_str("512x512");
        assert_eq!(sz.width, 512);
        assert_eq!(sz.height, 512);
    }

    #[test]
    fn image_size_parses_asymmetric() {
        let sz = ImageSize::from_size_str("1024x768");
        assert_eq!(sz.width, 1024);
        assert_eq!(sz.height, 768);
    }

    #[test]
    fn image_size_falls_back_on_garbage() {
        let sz = ImageSize::from_size_str("bad");
        assert_eq!(sz.width, 512);
        assert_eq!(sz.height, 512);
    }

    #[test]
    fn cancel_token_signals_correctly() {
        let tok = CancelToken::new();
        assert!(!tok.is_cancelled());
        tok.cancel();
        assert!(tok.is_cancelled());
    }

    #[test]
    fn cancel_token_clone_shares_state() {
        let tok = CancelToken::new();
        let tok2 = tok.clone();
        tok.cancel();
        assert!(tok2.is_cancelled());
    }
}
