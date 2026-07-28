//! Generic OpenAI-compatible `/v1/models` discovery.
//!
//! Parses model lists from OpenAI, Ollama, and bare array JSON shapes.
//! Provides best-effort HTTP discovery with timeout and optional API key support.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Response envelope shapes we support.
#[derive(Debug, Deserialize, Serialize)]
#[serde(untagged)]
enum Resp {
    /// OpenAI shape: `{data: [{id: "..."}]}`
    OpenAi { data: Vec<Entry> },
    /// Ollama shape: `{models: [{id: "..."}]}`
    Ollama { models: Vec<Entry> },
    /// Bare array: `[{id: "..."} | {name: "..."}]`
    Bare(Vec<Entry>),
}

/// Model entry with either `id` or `name` field.
#[derive(Debug, Deserialize, Serialize)]
struct Entry {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

impl Entry {
    /// Extract the model identifier, preferring `id` over `name`.
    fn ident(self) -> Option<String> {
        self.id.or(self.name)
    }
}

/// Parse a JSON response from `/v1/models` endpoint.
///
/// Handles three shapes:
/// - OpenAI: `{data: [{id: "model-id"}]}`
/// - Ollama: `{models: [{id: "model-id"} | {name: "model-name"}]}`
/// - Bare array: `[{id: "..."} | {name: "..."}]`
pub fn parse_models_response(json: &str) -> Vec<String> {
    let resp: Resp = match serde_json::from_str(json) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let entries = match resp {
        Resp::OpenAi { data } => data,
        Resp::Ollama { models } => models,
        Resp::Bare(v) => v,
    };

    entries.into_iter().filter_map(Entry::ident).collect()
}

/// Construct the `/v1/models` URL, handling bases that may already end with `/v1`.
fn models_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        format!("{trimmed}/models")
    } else {
        format!("{trimmed}/v1/models")
    }
}

/// Discover available models via HTTP GET to `{base}/v1/models`.
///
/// Best-effort with timeout. If API key is provided and non-empty, sends `Authorization: Bearer <key>`.
/// Returns model IDs extracted from the response envelope.
#[allow(dead_code)]
pub fn discover_models(
    base_url: &str,
    api_key: Option<&str>,
    timeout_secs: u64,
) -> Result<Vec<String>> {
    let url = models_url(base_url);
    let url_for_err = url.clone();
    let bearer = api_key
        .filter(|k| !k.is_empty())
        .map(|k| format!("Bearer {k}"));

    // `reqwest::blocking` panics if constructed inside a Tokio runtime context
    // (the CLI's `block_on`, or a `spawn_blocking` worker which carries a
    // runtime handle). Run it on a dedicated OS thread that has no ambient
    // runtime so the caller's context never matters.
    let body = std::thread::spawn(move || -> Result<String> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()?;
        let mut rb = client.get(url);
        if let Some(b) = bearer {
            rb = rb.header("Authorization", b);
        }
        // A 401/404 still has a body; without this check it parses to an empty
        // list and the caller reads "reachable, zero models" instead of "auth
        // failed". Fail loudly instead.
        let resp = rb.send()?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().unwrap_or_default();
            anyhow::bail!(
                "{status} from {}: {}",
                url_for_err,
                body.chars().take(200).collect::<String>()
            );
        }
        Ok(resp.text()?)
    })
    .join()
    .map_err(|_| anyhow::anyhow!("discovery thread panicked"))??;

    Ok(parse_models_response(&body))
}

/// Generate a stable registry alias for a model: `{provider}_{modelslug}`,
/// lowercased, with non-alphanumerics collapsed to underscore.
///
/// Example: `("anthropic", "claude-opus-4.8")` → `"anthropic_claude_opus_4_8"`
///
/// Wired by S3 Task 2/3.
#[allow(dead_code)]
pub fn default_alias(provider: &str, model_id: &str) -> String {
    let mut s = String::with_capacity(provider.len() + 1 + model_id.len());
    s.push_str(&provider.to_ascii_lowercase());
    s.push('_');

    for c in model_id.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else {
            s.push('_');
        }
    }

    // Collapse consecutive underscores.
    let mut result = String::new();
    let mut last_was_underscore = false;
    for c in s.chars() {
        if c == '_' {
            if !last_was_underscore {
                result.push(c);
            }
            last_was_underscore = true;
        } else {
            result.push(c);
            last_was_underscore = false;
        }
    }

    // Trim leading/trailing underscores.
    result.trim_matches('_').to_string()
}

/// Known local OpenAI-compatible runtimes probed during auto-detection.
pub struct LocalPreset {
    pub key: &'static str,
    pub name: &'static str,
    pub base_url: &'static str,
}

pub const LOCAL_PRESETS: &[LocalPreset] = &[
    LocalPreset {
        key: "ollama",
        name: "Ollama",
        base_url: "http://localhost:11434/v1",
    },
    LocalPreset {
        key: "mlx",
        name: "MLX (omlx)",
        base_url: "http://127.0.0.1:8000/v1",
    },
    LocalPreset {
        key: "lmstudio",
        name: "LM Studio",
        base_url: "http://localhost:1234/v1",
    },
];

/// A local runtime that answered the probe.
#[derive(Debug, Clone, Serialize)]
pub struct DetectedLocal {
    pub key: String,
    pub name: String,
    pub base_url: String,
    pub models: Vec<String>,
}

/// Probe each local preset; return those reachable. Best-effort, never panics.
#[allow(dead_code)] // wired by S3 Task 3
pub fn probe_local(timeout_secs: u64) -> Vec<DetectedLocal> {
    LOCAL_PRESETS
        .iter()
        .filter_map(|p| {
            // local servers need no key. Treat any successful HTTP response
            // (even an empty model list) as "reachable".
            match discover_models(p.base_url, None, timeout_secs) {
                Ok(models) => Some(DetectedLocal {
                    key: p.key.to_string(),
                    name: p.name.to_string(),
                    base_url: p.base_url.to_string(),
                    models,
                }),
                Err(_) => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_envelope() {
        let json = r#"{"data":[{"id":"gpt-4","object":"model"},{"id":"gpt-3.5-turbo"}]}"#;
        let models = parse_models_response(json);
        assert_eq!(models, vec!["gpt-4", "gpt-3.5-turbo"]);
    }

    #[test]
    fn parses_ollama_and_bare_shapes() {
        // Ollama shape
        let ollama_json = r#"{"models":[{"name":"llama2"},{"id":"mistral"}]}"#;
        let ollama_models = parse_models_response(ollama_json);
        assert_eq!(ollama_models, vec!["llama2", "mistral"]);

        // Bare array shape
        let bare_json = r#"[{"id":"model-a"},{"name":"model-b"}]"#;
        let bare_models = parse_models_response(bare_json);
        assert_eq!(bare_models, vec!["model-a", "model-b"]);
    }

    #[test]
    fn alias_slugs_punctuation() {
        assert_eq!(
            default_alias("anthropic", "claude-opus-4.8"),
            "anthropic_claude_opus_4_8"
        );
        assert_eq!(default_alias("openai", "gpt-4-turbo"), "openai_gpt_4_turbo");
        assert_eq!(
            default_alias("meta", "llama-2-70b-chat"),
            "meta_llama_2_70b_chat"
        );
        assert_eq!(
            default_alias("provider", "model@latest#v1"),
            "provider_model_latest_v1"
        );
        // S3 Task 1 mandated test vectors
        assert_eq!(
            default_alias("anthropic", "claude-opus-4-8"),
            "anthropic_claude_opus_4_8"
        );
        assert_eq!(
            default_alias("openrouter", "meta-llama/llama-4"),
            "openrouter_meta_llama_llama_4"
        );
        assert_eq!(default_alias("ollama", "qwen3:8b"), "ollama_qwen3_8b");
        // Lowercasing fix verification
        assert_eq!(
            default_alias("Anthropic", "Claude-Opus-4-8"),
            "anthropic_claude_opus_4_8"
        );
    }

    #[test]
    fn local_presets_cover_known_runtimes() {
        // Ensure the preset table includes all documented local runtimes.
        let keys: Vec<&str> = LOCAL_PRESETS.iter().map(|p| p.key).collect();
        assert!(keys.contains(&"ollama"), "ollama preset expected");
        assert!(keys.contains(&"mlx"), "mlx preset expected");
        assert!(keys.contains(&"lmstudio"), "lmstudio preset expected");

        // Ensure each preset has a non-empty base_url (with /v1 suffix).
        for preset in LOCAL_PRESETS {
            assert!(!preset.base_url.is_empty(), "base_url must not be empty");
            assert!(
                preset.base_url.ends_with("/v1"),
                "base_url must end with /v1"
            );
        }
    }

    /// One-shot HTTP server on an ephemeral port; serves `status_line` + `body`
    /// to a single client and returns the base URL to aim `discover_models` at.
    /// ponytail: stdlib TcpListener, not a mock-server dependency.
    fn one_shot_server(status_line: &'static str, body: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.read(&mut [0u8; 1024]);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        format!("http://127.0.0.1:{port}/v1")
    }

    #[test]
    fn non_success_status_errors_instead_of_reading_as_zero_models() {
        let base = one_shot_server("401 Unauthorized", r#"{"error":{"message":"bad key"}}"#);
        let err = discover_models(&base, Some("nope"), 5)
            .expect_err("a 401 must not parse into an empty model list")
            .to_string();
        assert!(err.contains("401"), "status must reach the caller: {err}");
        assert!(err.contains("bad key"), "body must reach the caller: {err}");
    }

    #[test]
    fn success_status_still_parses_the_envelope() {
        let base = one_shot_server("200 OK", r#"{"data":[{"id":"gpt-4"}]}"#);
        assert_eq!(discover_models(&base, None, 5).unwrap(), vec!["gpt-4"]);
    }

    #[test]
    fn probe_local_handles_unreachable_without_panic() {
        // With a short timeout (1s), localhost ports 11434/8000/1234 are likely
        // unreachable and will time out. Ensure probe_local never panics and
        // returns an empty list (or at least a list with len ≤ 3).
        let result = probe_local(1);
        // Should not panic. Result may be empty or partially populated if
        // a local server happens to be running, but length should be ≤ 3.
        assert!(result.len() <= 3, "probe_local returned too many results");
        for detected in &result {
            assert!(!detected.key.is_empty(), "detected.key must not be empty");
            assert!(
                !detected.base_url.is_empty(),
                "detected.base_url must not be empty"
            );
        }
    }
}
