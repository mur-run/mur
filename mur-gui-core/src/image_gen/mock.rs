use super::{CancelToken, ImageGenError, ImageGenProvider, ImageSize};
use async_trait::async_trait;
use image::{DynamicImage, ImageBuffer, Rgba};
use std::path::Path;

/// Synthetic image provider for tests and CI. Generates a solid-colored
/// placeholder image in under a millisecond — no network, no API key.
pub struct MockImageGenProvider;

#[async_trait]
impl ImageGenProvider for MockImageGenProvider {
    async fn generate(
        &self,
        prompt: &str,
        _negative: Option<&str>,
        size: ImageSize,
        _source_image: Option<&Path>,
        cancel: CancelToken,
    ) -> Result<DynamicImage, ImageGenError> {
        if cancel.is_cancelled() {
            return Err(ImageGenError::Cancelled);
        }
        Ok(DynamicImage::ImageRgba8(placeholder_image(prompt, size)))
    }

    fn name(&self) -> &str {
        "mock"
    }
}

fn placeholder_image(prompt: &str, size: ImageSize) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    let color = prompt_color(prompt);
    ImageBuffer::from_fn(size.width, size.height, |_, _| Rgba(color))
}

/// Deterministic pastel color derived from the prompt string.
fn prompt_color(prompt: &str) -> [u8; 4] {
    let h = prompt
        .bytes()
        .fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    // Keep saturation low for a pleasant mock palette.
    let r = 180u8.saturating_add((h & 0x3F) as u8);
    let g = 180u8.saturating_add(((h >> 6) & 0x3F) as u8);
    let b = 180u8.saturating_add(((h >> 12) & 0x3F) as u8);
    [r, g, b, 255]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_returns_image_of_requested_size() {
        let provider = MockImageGenProvider;
        let img = provider
            .generate(
                "test prompt idle",
                None,
                ImageSize { width: 64, height: 64 },
                None,
                CancelToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(img.width(), 64);
        assert_eq!(img.height(), 64);
    }

    #[tokio::test]
    async fn mock_respects_cancellation() {
        let provider = MockImageGenProvider;
        let cancel = CancelToken::new();
        cancel.cancel();
        let result = provider
            .generate("anything", None, ImageSize { width: 64, height: 64 }, None, cancel)
            .await;
        assert!(matches!(result, Err(ImageGenError::Cancelled)));
    }

    #[test]
    fn placeholder_colors_differ_for_different_prompts() {
        let c1 = prompt_color("idle");
        let c2 = prompt_color("smile");
        assert_ne!(c1, c2);
    }
}
