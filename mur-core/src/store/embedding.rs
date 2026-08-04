//! Embedding generation via Ollama or OpenAI API.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Embedding provider configuration.
#[derive(Debug, Clone)]
pub struct EmbeddingConfig {
    pub provider: EmbeddingProvider,
    pub model: String,
    #[allow(dead_code)] // Used by callers to pass dimensions to VectorStore
    pub dimensions: usize,
    /// Max texts per embedding API request (controls model-server RAM).
    pub batch_size: usize,
}

#[derive(Debug, Clone)]
pub enum EmbeddingProvider {
    Ollama {
        base_url: String,
    },
    OpenAI {
        api_key: String,
        base_url: String,
        /// Why the key is empty, when a key *was* configured but could not be
        /// resolved. `None` means no key was configured at all — a legitimate
        /// setup for a local OpenAI-compatible server that takes no auth.
        ///
        /// Carried all the way to the HTTP error so the reason travels with
        /// the failure instead of only reaching a log the caller never reads
        /// (an MCP server's stderr is drained at `debug` by its supervisor).
        key_hint: Option<String>,
    },
}

/// Resolve the OpenAI-compatible API key from `api_key_ref`, then
/// `api_key_env`, then `OPENAI_API_KEY`.
///
/// Returns the key plus a diagnostic hint. An empty key with no hint is
/// legitimate (a local server with auth disabled); an empty key *with* a hint
/// is a misconfiguration that used to degrade silently into sending
/// `Authorization: Bearer ` and getting an unexplained 401 back.
fn resolve_api_key(cfg: &mur_common::config::Config) -> (String, Option<String>) {
    let emb = &cfg.embedding;
    let mut misses: Vec<String> = Vec::new();

    if let Some(raw) = emb.api_key_ref.as_deref().filter(|s| !s.trim().is_empty()) {
        match raw.parse::<mur_common::secret::SecretRef>() {
            Ok(sref) => match sref.resolve_to_string_blocking() {
                Some(key) if !key.is_empty() => return (key, None),
                Some(_) => misses.push(format!("api_key_ref `{raw}` resolved to an empty secret")),
                // Re-resolve purely to recover the error text for the hint;
                // only ever runs on the path that already failed.
                None => {
                    let why = sref
                        .resolve_blocking()
                        .err()
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "not found".into());
                    misses.push(format!("api_key_ref `{raw}` did not resolve ({why})"));
                }
            },
            Err(e) => misses.push(format!(
                "api_key_ref `{raw}` is not a valid secret ref ({e})"
            )),
        }
    }

    if let Some(name) = emb.api_key_env.as_deref().filter(|s| !s.trim().is_empty()) {
        match std::env::var(name) {
            Ok(v) if !v.is_empty() => {
                if !misses.is_empty() {
                    tracing::warn!(
                        misses = misses.join("; "),
                        env = name,
                        "embedding: falling back to api_key_env"
                    );
                }
                return (v, None);
            }
            _ => misses.push(format!("api_key_env `{name}` is unset in this process")),
        }
    }

    if let Ok(v) = std::env::var("OPENAI_API_KEY")
        && !v.is_empty()
    {
        return (v, None);
    }

    if misses.is_empty() {
        return (String::new(), None);
    }

    let hint = format!(
        "no embedding API key: {}. A background agent process cannot always \
         reach the OS keychain — set the key in the environment of whatever \
         launches it, or run `mur doctor`",
        misses.join("; ")
    );
    tracing::warn!("{hint}");
    (String::new(), Some(hint))
}

impl EmbeddingConfig {
    /// Create from the global mur config.
    pub fn from_config(cfg: &mur_common::config::Config) -> Self {
        let provider = match cfg.embedding.provider.as_str() {
            "openai" | "gemini" | "anthropic" | "voyage" | "omlx" | "mlx" => {
                let (api_key, key_hint) = resolve_api_key(cfg);
                let base_url = cfg
                    .embedding
                    .openai_url
                    .clone()
                    .unwrap_or_else(|| "https://api.openai.com/v1".into());
                EmbeddingProvider::OpenAI {
                    api_key,
                    base_url,
                    key_hint,
                }
            }
            _ => EmbeddingProvider::Ollama {
                base_url: cfg
                    .embedding
                    .ollama_endpoint
                    .clone()
                    .unwrap_or_else(|| mur_common::config::DEFAULT_OLLAMA_ENDPOINT.to_string()),
            },
        };
        Self {
            provider,
            model: cfg.embedding.model.clone(),
            dimensions: cfg.embedding.dimensions,
            batch_size: cfg.sources_global.embedding_batch_size,
        }
    }
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: EmbeddingProvider::Ollama {
                base_url: "http://localhost:11434".into(),
            },
            model: "qwen3-embedding:0.6b".into(),
            dimensions: 1024,
            batch_size: 32,
        }
    }
}

/// Generate an embedding vector for the given text.
pub async fn embed(text: &str, config: &EmbeddingConfig) -> Result<Vec<f32>> {
    match &config.provider {
        EmbeddingProvider::Ollama { base_url } => embed_ollama(text, base_url, &config.model).await,
        EmbeddingProvider::OpenAI {
            api_key,
            base_url,
            key_hint,
        } => embed_openai(text, base_url, api_key, key_hint.as_deref(), &config.model).await,
    }
}

/// Batch embed multiple texts. Uses native batch API for Ollama, sequential for OpenAI.
pub async fn embed_batch(texts: &[String], config: &EmbeddingConfig) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    match &config.provider {
        EmbeddingProvider::Ollama { base_url } => {
            embed_ollama_batch(texts, base_url, &config.model).await
        }
        EmbeddingProvider::OpenAI {
            api_key,
            base_url,
            key_hint,
        } => embed_openai_batch(texts, base_url, api_key, key_hint.as_deref(), &config.model).await,
    }
}

// ─── Ollama ──────────────────────────────────────────────────────

/// How long Ollama should keep the embedding model resident after a request.
/// Without this, Ollama unloads the model after its default idle timeout, so
/// every `mur project search` after a pause pays a multi-second model-load cost.
/// Keeping it warm makes repeated searches feel interactive.
const EMBED_KEEP_ALIVE: &str = "15m";

#[derive(Serialize)]
struct OllamaEmbedRequest {
    model: String,
    input: String,
    keep_alive: &'static str,
}

#[derive(Deserialize)]
struct OllamaEmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

async fn embed_ollama(text: &str, base_url: &str, model: &str) -> Result<Vec<f32>> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/embed", base_url))
        .json(&OllamaEmbedRequest {
            model: model.into(),
            input: text.into(),
            keep_alive: EMBED_KEEP_ALIVE,
        })
        .send()
        .await
        .context("calling Ollama embed API")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Ollama API error {}: {}", status, body);
    }

    let data: OllamaEmbedResponse = resp.json().await.context("parsing Ollama response")?;
    data.embeddings
        .into_iter()
        .next()
        .context("no embedding returned")
}

async fn embed_ollama_batch(
    texts: &[String],
    base_url: &str,
    model: &str,
) -> Result<Vec<Vec<f32>>> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/embed", base_url))
        .json(&serde_json::json!({
            "model": model,
            "input": texts,
            "keep_alive": EMBED_KEEP_ALIVE,
        }))
        .send()
        .await
        .context("calling Ollama batch embed API")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Ollama API error {}: {}", status, body);
    }

    let data: OllamaEmbedResponse = resp.json().await.context("parsing Ollama batch response")?;
    Ok(data.embeddings)
}

// ─── OpenAI ─────────────────────────────────────────────────────

#[derive(Serialize)]
struct OpenAIEmbedRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Deserialize)]
struct OpenAIEmbedResponse {
    data: Vec<OpenAIEmbedData>,
}

#[derive(Deserialize)]
struct OpenAIEmbedData {
    embedding: Vec<f32>,
}

/// Attach `Authorization` only when a key is actually present.
///
/// An empty key means no auth was configured; sending `Bearer ` turns a
/// working no-auth local server into a 401.
fn with_auth(req: reqwest::RequestBuilder, api_key: &str) -> reqwest::RequestBuilder {
    if api_key.is_empty() {
        req
    } else {
        req.header("Authorization", format!("Bearer {api_key}"))
    }
}

fn embed_error(
    status: reqwest::StatusCode,
    url: &str,
    body: &str,
    key_hint: Option<&str>,
) -> anyhow::Error {
    match key_hint {
        Some(h) => anyhow::anyhow!("Embed API error {status} at {url}: {body} — {h}"),
        None => anyhow::anyhow!("Embed API error {status} at {url}: {body}"),
    }
}

async fn embed_openai(
    text: &str,
    base_url: &str,
    api_key: &str,
    key_hint: Option<&str>,
    model: &str,
) -> Result<Vec<f32>> {
    let client = reqwest::Client::new();
    let url = format!("{}/embeddings", base_url.trim_end_matches('/'));
    let resp = with_auth(client.post(&url), api_key)
        .json(&OpenAIEmbedRequest {
            model: model.into(),
            input: vec![text.to_string()],
        })
        .send()
        .await
        .with_context(|| format!("calling embed API at {}", url))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(embed_error(status, &url, &body, key_hint));
    }

    let data: OpenAIEmbedResponse = resp.json().await.context("parsing embed response")?;
    data.data
        .into_iter()
        .next()
        .map(|d| d.embedding)
        .context("no embedding returned")
}

async fn embed_openai_batch(
    texts: &[String],
    base_url: &str,
    api_key: &str,
    key_hint: Option<&str>,
    model: &str,
) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let client = reqwest::Client::new();
    let url = format!("{}/embeddings", base_url.trim_end_matches('/'));
    let resp = with_auth(client.post(&url), api_key)
        .json(&OpenAIEmbedRequest {
            model: model.into(),
            input: texts.to_vec(),
        })
        .send()
        .await
        .with_context(|| format!("calling embed API at {}", url))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(embed_error(status, &url, &body, key_hint));
    }

    let data: OpenAIEmbedResponse = resp.json().await.context("parsing embed response")?;
    let embeddings: Vec<Vec<f32>> = data.data.into_iter().map(|d| d.embedding).collect();
    if embeddings.len() != texts.len() {
        anyhow::bail!(
            "Embed API returned {} embeddings but {} were requested",
            embeddings.len(),
            texts.len()
        );
    }
    Ok(embeddings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::config::Config;

    fn cfg_with(provider: &str, url: Option<&str>, env: Option<&str>) -> Config {
        let mut cfg = Config::default();
        cfg.embedding.provider = provider.into();
        cfg.embedding.openai_url = url.map(str::to_string);
        cfg.embedding.api_key_env = env.map(str::to_string);
        cfg
    }

    #[test]
    fn omlx_provider_uses_custom_base_url() {
        let cfg = cfg_with(
            "omlx",
            Some("http://localhost:8000/v1"),
            Some("OMLX_API_KEY"),
        );
        let ec = EmbeddingConfig::from_config(&cfg);
        match ec.provider {
            EmbeddingProvider::OpenAI { base_url, .. } => {
                assert_eq!(base_url, "http://localhost:8000/v1");
            }
            _ => panic!("expected OpenAI variant for provider=omlx"),
        }
    }

    #[test]
    fn openai_provider_defaults_to_canonical_base_url() {
        let cfg = cfg_with("openai", None, Some("OPENAI_API_KEY"));
        let ec = EmbeddingConfig::from_config(&cfg);
        match ec.provider {
            EmbeddingProvider::OpenAI { base_url, .. } => {
                assert_eq!(base_url, "https://api.openai.com/v1");
            }
            _ => panic!("expected OpenAI variant"),
        }
    }

    #[test]
    fn ollama_provider_unchanged_by_openai_url() {
        let cfg = cfg_with("ollama", Some("http://example.com"), None);
        let ec = EmbeddingConfig::from_config(&cfg);
        assert!(
            matches!(ec.provider, EmbeddingProvider::Ollama { .. }),
            "ollama provider must produce Ollama variant regardless of openai_url"
        );
    }

    #[test]
    fn from_config_prefers_api_key_ref() {
        unsafe { std::env::set_var("MUR_TEST_EMB_REF", "emb-key") };
        let mut cfg = mur_common::config::Config::default();
        cfg.embedding.provider = "openai".into();
        cfg.embedding.api_key_ref = Some("env:MUR_TEST_EMB_REF".into());
        let ec = EmbeddingConfig::from_config(&cfg);
        match ec.provider {
            EmbeddingProvider::OpenAI {
                api_key, key_hint, ..
            } => {
                assert_eq!(api_key, "emb-key");
                assert!(key_hint.is_none(), "a resolved key must carry no hint");
            }
            _ => panic!("expected OpenAI provider"),
        }
        unsafe { std::env::remove_var("MUR_TEST_EMB_REF") };
    }

    /// The bug behind the unexplained `401 API key required`: a configured
    /// `api_key_ref` that fails to resolve (a background agent process that
    /// cannot reach the keychain) used to fall through to an empty key and
    /// send `Authorization: Bearer `. The reason must survive to the caller.
    ///
    /// Both env-key tests below blank `OPENAI_API_KEY` rather than removing
    /// it, so they are deterministic whatever the ambient environment holds.
    /// Nothing else in this crate reads that variable.
    #[test]
    fn unresolvable_key_ref_yields_hint_instead_of_silent_empty_key() {
        unsafe {
            std::env::set_var("OPENAI_API_KEY", "");
            std::env::remove_var("MUR_TEST_EMB_ABSENT");
        }
        let mut cfg = mur_common::config::Config::default();
        cfg.embedding.provider = "omlx".into();
        cfg.embedding.api_key_ref = Some("env:MUR_TEST_EMB_ABSENT".into());
        cfg.embedding.api_key_env = Some("MUR_TEST_EMB_ABSENT".into());
        match EmbeddingConfig::from_config(&cfg).provider {
            EmbeddingProvider::OpenAI {
                api_key, key_hint, ..
            } => {
                assert!(api_key.is_empty());
                let hint = key_hint.expect("unresolvable ref must produce a hint");
                assert!(
                    hint.contains("env:MUR_TEST_EMB_ABSENT"),
                    "hint must name the ref that failed, got: {hint}"
                );
                assert!(
                    hint.contains("MUR_TEST_EMB_ABSENT` is unset"),
                    "hint must also report the env fallback miss, got: {hint}"
                );
            }
            _ => panic!("expected OpenAI provider"),
        }
    }

    /// No key configured at all is a legitimate setup (a local
    /// OpenAI-compatible server with auth disabled) — no hint, and the
    /// request must go out without an `Authorization` header.
    #[test]
    fn no_key_configured_is_not_an_error() {
        unsafe { std::env::set_var("OPENAI_API_KEY", "") };
        let mut cfg = mur_common::config::Config::default();
        cfg.embedding.provider = "omlx".into();
        match EmbeddingConfig::from_config(&cfg).provider {
            EmbeddingProvider::OpenAI {
                api_key, key_hint, ..
            } => {
                assert!(api_key.is_empty());
                assert!(key_hint.is_none(), "an unconfigured key is not a failure");
            }
            _ => panic!("expected OpenAI provider"),
        }
    }

    #[test]
    fn with_auth_omits_header_when_key_is_empty() {
        let client = reqwest::Client::new();
        let built = with_auth(client.post("http://127.0.0.1/v1/embeddings"), "")
            .build()
            .unwrap();
        assert!(
            built.headers().get("Authorization").is_none(),
            "an empty key must send no Authorization header"
        );
        let built = with_auth(client.post("http://127.0.0.1/v1/embeddings"), "k")
            .build()
            .unwrap();
        assert_eq!(built.headers()["Authorization"], "Bearer k");
    }
}
