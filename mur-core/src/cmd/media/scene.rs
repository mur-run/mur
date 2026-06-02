//! scene-explain: capture the current VLC frame and explain it with the local
//! multimodal model.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Return the most recently modified regular file in `dir`, if any.
pub fn newest_file(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let mtime = entry.metadata().ok()?.modified().ok()?;
        if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
            best = Some((mtime, path));
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn newest_file_picks_latest() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.png"), b"a").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.path().join("b.png"), b"b").unwrap();
        assert_eq!(
            newest_file(dir.path()).unwrap().file_name().unwrap(),
            "b.png"
        );
    }

    #[test]
    fn newest_file_empty_dir_is_none() {
        let dir = TempDir::new().unwrap();
        assert!(newest_file(dir.path()).is_none());
    }
}

// ── VLM vision request / response ──

use serde_json::{Value, json};

/// Default instruction when the caller gives no prompt.
pub const DEFAULT_EXPLAIN_PROMPT: &str =
    "用繁體中文、溫暖簡潔地說明這個畫面正在發生什麼；若有人物或字幕，也一併解讀。";

/// Build an OpenAI-compatible vision chat request body.
pub fn build_request(model: &str, prompt: &str, image_data_url: &str) -> Value {
    json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": prompt },
                { "type": "image_url", "image_url": { "url": image_data_url } }
            ]
        }],
        "max_tokens": 512
    })
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
