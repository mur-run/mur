//! scene-explain: capture the current VLC frame and explain it with the local
//! multimodal model.

use anyhow::{Context, Result};
use std::time::Duration;

// Snapshot-selection helpers live in `mur-common::media` so the runtime's
// WatchScheduler (which cannot depend on mur-core) shares them. Their tests
// live there too.
use mur_common::media::newest_file;
use mur_common::media::newest_file_excluding;

// ── VLM vision request / response ──

use serde_json::{Value, json};

/// Default instruction when the caller gives no prompt.
pub const DEFAULT_EXPLAIN_PROMPT: &str =
    "用繁體中文、溫暖簡潔地說明這個畫面正在發生什麼；若有人物或字幕，也一併解讀。";

/// Build an OpenAI-compatible vision chat request body.
///
/// The bundled Qwen3 reasoning model needs `chat_template_kwargs.enable_thinking=false`
/// so the answer lands in `message.content`; that rule lives in one place
/// (`analyze::disable_thinking_for_bundled`) and is gated on the model so non-Qwen
/// endpoints don't receive an unknown field.
pub fn build_request(model: &str, prompt: &str, image_data_url: &str) -> Value {
    let mut body = json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": prompt },
                { "type": "image_url", "image_url": { "url": image_data_url } }
            ]
        }],
        "max_tokens": 512
    });
    super::analyze::disable_thinking_for_bundled(&mut body, model);
    body
}

/// Encode PNG bytes as a data URL for the image_url field.
pub fn png_data_url(bytes: &[u8]) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("data:image/png;base64,{b64}")
}

/// Extract assistant text from an OpenAI-compatible chat completion response.
pub fn parse_completion(resp: &Value) -> Option<String> {
    resp.get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?
        .as_str()
        .map(|s| s.to_string())
}

// ── Orchestrator ──

use mur_common::config::DEFAULT_BUNDLED_MODEL_ID;

/// Resolve the local model endpoint base URL (e.g. http://127.0.0.1:PORT/v1).
fn local_base_url() -> Result<String> {
    let home = crate::cmd::resolve_mur_home()?;
    mur_common::local_llm::read_base_url(&home)
        .context("local model endpoint not available (is MuR Hub running?)")
}

/// Capture the current VLC frame and explain it with the local multimodal model.
#[allow(dead_code)]
pub async fn explain(prompt: Option<&str>) -> Result<String> {
    let client = super::shared_client();

    // 1. Ensure VLC is up and take a snapshot.
    let rt = super::vlc::ensure_for_snapshot(client).await?;
    // Record the pre-capture snapshot so we can require a *newer* (differently
    // named) frame — never an older snapshot from a prior session. Path-based so
    // it's robust to filesystem mtime granularity (see `newest_file_excluding`).
    let baseline = newest_file(&rt.snapshot_dir);
    super::vlc::snapshot_command(&rt, client).await?;

    // 2. Read the newest snapshot file (retry briefly for the file to land).
    let mut img_path = None;
    for i in 0..10 {
        if let Some(p) = newest_file_excluding(&rt.snapshot_dir, baseline.as_deref()) {
            img_path = Some(p);
            break;
        }
        // Periodically verify VLC is still alive before retrying.
        if i > 0 && i % 3 == 0 {
            let alive = client
                .get(super::vlc::status_url(rt.port))
                .basic_auth("", Some(&rt.password))
                .timeout(Duration::from_secs(2))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false);
            if !alive {
                anyhow::bail!("VLC disconnected while waiting for snapshot");
            }
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    // No fresh frame means VLC produced no new snapshot (typically: nothing is
    // playing). Fail rather than describe a stale frame from a previous session.
    let img_path = img_path.context("no fresh snapshot produced by VLC (is something playing?)")?;

    // Handle race: VLC may replace the snapshot file between discovery and read.
    let bytes = match std::fs::read(&img_path) {
        Ok(b) => b,
        Err(_) => {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let retry = newest_file_excluding(&rt.snapshot_dir, baseline.as_deref())
                .context("no snapshot (file vanished and no replacement found)")?;
            std::fs::read(&retry).context("read snapshot")?
        }
    };

    // 3. Call the local OpenAI-compatible vision endpoint.
    let base = local_base_url()?;
    let body = build_request(
        DEFAULT_BUNDLED_MODEL_ID,
        prompt.unwrap_or(DEFAULT_EXPLAIN_PROMPT),
        &png_data_url(&bytes),
    );
    let resp: serde_json::Value = client
        .post(format!("{}/chat/completions", base.trim_end_matches('/')))
        .json(&body)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .context("call local VLM")?
        .json()
        .await
        .context("parse VLM response")?;

    parse_completion(&resp).context("VLM returned no content")
}

#[cfg(test)]
mod request_tests {
    use super::*;

    #[test]
    fn request_has_text_and_image_parts() {
        let body = build_request("Qwen3.5-2B-MLX-4bit", "hi", "data:image/png;base64,AAA");
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AAA");
    }

    #[test]
    fn data_url_prefixed() {
        assert!(png_data_url(b"\x89PNG").starts_with("data:image/png;base64,"));
    }

    #[test]
    fn parse_extracts_content() {
        let resp = serde_json::json!({
            "choices": [{ "message": { "content": "這是一隻貓" } }]
        });
        assert_eq!(parse_completion(&resp).as_deref(), Some("這是一隻貓"));
    }
}
