use super::{CancelToken, ImageGenError, ImageGenProvider, ImageSize};
use async_trait::async_trait;
use base64::Engine as _;
use image::DynamicImage;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Gemini image-generation provider.
///
/// Uses the `generateContent` endpoint with `responseModalities: ["IMAGE"]`.
/// API key is passed directly; secret resolution via `mur model` registry
/// is wired in M-h7 Hub settings.
pub struct GeminiImageGenProvider {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl GeminiImageGenProvider {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            client: reqwest::Client::new(),
        }
    }

    fn endpoint(&self) -> String {
        format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent",
            self.model
        )
    }
}

// ─── Gemini REST types ──────────────────────────────────────────────────────

#[derive(Serialize)]
struct GenerateRequest<'a> {
    contents: Vec<Content<'a>>,
    #[serde(rename = "generationConfig")]
    generation_config: GenConfig,
}

#[derive(Serialize)]
struct Content<'a> {
    parts: Vec<Part<'a>>,
}

#[derive(Serialize)]
struct Part<'a> {
    text: &'a str,
}

#[derive(Serialize)]
struct GenConfig {
    #[serde(rename = "responseModalities")]
    response_modalities: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct GenerateResponse {
    candidates: Option<Vec<Candidate>>,
    error: Option<ApiError>,
}

#[derive(Deserialize, Debug)]
struct Candidate {
    content: Option<RespContent>,
}

#[derive(Deserialize, Debug)]
struct RespContent {
    parts: Vec<RespPart>,
}

#[derive(Deserialize, Debug)]
struct RespPart {
    #[serde(rename = "inlineData")]
    inline_data: Option<InlineData>,
}

#[derive(Deserialize, Debug)]
struct InlineData {
    #[serde(rename = "mimeType")]
    #[allow(dead_code)]
    mime_type: String,
    data: String, // base64
}

#[derive(Deserialize, Debug)]
struct ApiError {
    message: String,
}

// ─── Provider impl ──────────────────────────────────────────────────────────

#[async_trait]
impl ImageGenProvider for GeminiImageGenProvider {
    async fn generate(
        &self,
        prompt: &str,
        negative: Option<&str>,
        _size: ImageSize,
        _source_image: Option<&Path>,
        cancel: CancelToken,
    ) -> Result<DynamicImage, ImageGenError> {
        if cancel.is_cancelled() {
            return Err(ImageGenError::Cancelled);
        }

        let full_prompt = match negative {
            Some(neg) => format!("{prompt}\nNegative: {neg}"),
            None => prompt.to_string(),
        };

        let body = GenerateRequest {
            contents: vec![Content {
                parts: vec![Part { text: &full_prompt }],
            }],
            generation_config: GenConfig {
                response_modalities: vec!["IMAGE".into()],
            },
        };

        let resp: GenerateResponse = self
            .client
            .post(self.endpoint())
            .query(&[("key", &self.api_key)])
            .json(&body)
            .send()
            .await?
            .json()
            .await?;

        if cancel.is_cancelled() {
            return Err(ImageGenError::Cancelled);
        }

        if let Some(err) = resp.error {
            return Err(ImageGenError::ApiError(err.message));
        }

        let inline = resp
            .candidates
            .as_deref()
            .and_then(|c| c.first())
            .and_then(|c| c.content.as_ref())
            .and_then(|c| c.parts.first())
            .and_then(|p| p.inline_data.as_ref())
            .ok_or_else(|| ImageGenError::ApiError("no image data in response".into()))?;

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&inline.data)
            .map_err(|e| ImageGenError::Decode(e.to_string()))?;

        let img =
            image::load_from_memory(&bytes).map_err(|e| ImageGenError::Decode(e.to_string()))?;

        Ok(img)
    }

    fn name(&self) -> &str {
        "gemini"
    }

    fn estimated_cost_per_image_usd(&self) -> f64 {
        // Gemini 2.5 Flash image generation: approximately $0.003 per image.
        0.003
    }
}
