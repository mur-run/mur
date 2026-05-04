//! Per-call cost telemetry: writes one JSONL record per LLM call to
//! `~/.mur/telemetry/llm-calls-<YYYY-MM-DD>.jsonl`. See spec §11 + plan task 8.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use super::{ChatBackend, ChatRequest, ChatResponse, ChatStream};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct LlmCallRecord {
    pub ts: chrono::DateTime<chrono::Utc>,
    pub provider: String,
    pub model: String,
    pub stage: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub latency_ms: u64,
    pub stream: bool,
    pub success: bool,
}

pub struct TelemetryBackend {
    inner: Arc<dyn ChatBackend>,
    stage: &'static str,
    /// Test override: when Some, records are appended here instead of the
    /// default ~/.mur/telemetry path.
    log_path_override: Option<PathBuf>,
}

impl TelemetryBackend {
    pub fn new(inner: Arc<dyn ChatBackend>, stage: &'static str) -> Self {
        Self {
            inner,
            stage,
            log_path_override: None,
        }
    }

    /// Test-only path override; integration tests redirect telemetry writes
    /// to a tempdir. Lib build sees no in-tree caller hence the allow.
    #[allow(dead_code)]
    pub fn with_path_override(mut self, path: PathBuf) -> Self {
        self.log_path_override = Some(path);
        self
    }

    fn write_record(&self, rec: &LlmCallRecord) {
        write_record_to_path(self.log_path_override.clone(), rec);
    }
}

#[async_trait]
impl ChatBackend for TelemetryBackend {
    async fn generate(&self, req: ChatRequest<'_>) -> Result<ChatResponse> {
        let start = Instant::now();
        let model = req.model.to_string();
        let provider = self.inner.provider_name().to_string();
        let result = self.inner.generate(req).await;
        let latency_ms = start.elapsed().as_millis() as u64;
        match &result {
            Ok(resp) => {
                self.write_record(&LlmCallRecord {
                    ts: Utc::now(),
                    provider,
                    model,
                    stage: self.stage.to_string(),
                    input_tokens: resp.usage.input_tokens,
                    output_tokens: resp.usage.output_tokens,
                    cache_creation_input_tokens: resp.usage.cache_creation_input_tokens,
                    cache_read_input_tokens: resp.usage.cache_read_input_tokens,
                    latency_ms,
                    stream: false,
                    success: true,
                });
            }
            Err(_) => {
                self.write_record(&LlmCallRecord {
                    ts: Utc::now(),
                    provider,
                    model,
                    stage: self.stage.to_string(),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    latency_ms,
                    stream: false,
                    success: false,
                });
            }
        }
        result
    }

    async fn generate_stream(&self, req: ChatRequest<'_>) -> Result<ChatStream> {
        use futures::stream::StreamExt;
        let start = Instant::now();
        let model = req.model.to_string();
        let provider = self.inner.provider_name().to_string();
        let stage = self.stage;
        let path_override = self.log_path_override.clone();
        let inner_stream = match self.inner.generate_stream(req).await {
            Ok(s) => s,
            Err(e) => {
                let rec = LlmCallRecord {
                    ts: Utc::now(),
                    provider,
                    model,
                    stage: stage.to_string(),
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                    latency_ms: start.elapsed().as_millis() as u64,
                    stream: true,
                    success: false,
                };
                write_record_to_path(path_override, &rec);
                return Err(e);
            }
        };
        let stream = futures::stream::unfold(
            (
                inner_stream,
                model,
                provider,
                stage,
                path_override,
                start,
                false,
            ),
            |(mut s, model, provider, stage, path_override, start, done)| async move {
                if done {
                    return None;
                }
                match s.next().await {
                    Some(Ok(chunk)) => {
                        let final_usage = chunk.usage.clone();
                        let item = Ok(chunk);
                        if let Some(u) = final_usage {
                            let rec = LlmCallRecord {
                                ts: Utc::now(),
                                provider: provider.clone(),
                                model: model.clone(),
                                stage: stage.to_string(),
                                input_tokens: u.input_tokens,
                                output_tokens: u.output_tokens,
                                cache_creation_input_tokens: u.cache_creation_input_tokens,
                                cache_read_input_tokens: u.cache_read_input_tokens,
                                latency_ms: start.elapsed().as_millis() as u64,
                                stream: true,
                                success: true,
                            };
                            write_record_to_path(path_override.clone(), &rec);
                        }
                        Some((
                            item,
                            (s, model, provider, stage, path_override, start, false),
                        ))
                    }
                    Some(Err(e)) => {
                        let rec = LlmCallRecord {
                            ts: Utc::now(),
                            provider: provider.clone(),
                            model: model.clone(),
                            stage: stage.to_string(),
                            input_tokens: 0,
                            output_tokens: 0,
                            cache_creation_input_tokens: 0,
                            cache_read_input_tokens: 0,
                            latency_ms: start.elapsed().as_millis() as u64,
                            stream: true,
                            success: false,
                        };
                        write_record_to_path(path_override.clone(), &rec);
                        Some((
                            Err(e),
                            (s, model, provider, stage, path_override, start, true),
                        ))
                    }
                    None => None,
                }
            },
        );
        Ok(Box::pin(stream))
    }

    fn provider_name(&self) -> &'static str {
        self.inner.provider_name()
    }

    fn supports_caching(&self) -> bool {
        self.inner.supports_caching()
    }
}

fn default_log_path() -> PathBuf {
    let date = Utc::now().format("%Y-%m-%d");
    super::super::paths::telemetry_root(None).join(format!("llm-calls-{date}.jsonl"))
}

fn write_record_to_path(path_override: Option<PathBuf>, rec: &LlmCallRecord) {
    let path = path_override.unwrap_or_else(default_log_path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let line = match serde_json::to_string(rec) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(err = ?e, "failed to serialize LlmCallRecord");
            return;
        }
    };
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            if let Err(e) = writeln!(f, "{line}") {
                tracing::warn!(err = ?e, path = ?path, "failed to write telemetry line");
            }
        }
        Err(e) => {
            tracing::warn!(err = ?e, path = ?path, "failed to open telemetry file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversations::backend::{ChatChunk, Usage, mock::MockBackend};

    fn req() -> ChatRequest<'static> {
        ChatRequest {
            model: "mock-model",
            system: None,
            user: "mock extractive span",
            max_tokens: 64,
            temperature: None,
            stop: vec![],
            cache_system: false,
            cache_user_prefix: None,
        }
    }

    #[tokio::test]
    async fn generate_writes_one_jsonl_record_with_usage() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test-calls.jsonl");
        let inner = Arc::new(MockBackend::new());
        let tb = TelemetryBackend::new(inner, "extractive").with_path_override(path.clone());

        let _ = tb.generate(req()).await.unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.lines().count(), 1, "expected exactly one record line");
        let rec: LlmCallRecord = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert_eq!(rec.provider, "mock");
        assert_eq!(rec.stage, "extractive");
        assert!(!rec.stream);
        assert!(rec.success);
        assert!(rec.latency_ms < 5_000);
    }

    #[tokio::test]
    async fn generate_stream_writes_record_when_final_chunk_carries_usage() {
        struct OneChunkWithUsage;
        #[async_trait]
        impl ChatBackend for OneChunkWithUsage {
            async fn generate(&self, _: ChatRequest<'_>) -> Result<ChatResponse> {
                anyhow::bail!("unused")
            }
            async fn generate_stream(&self, _: ChatRequest<'_>) -> Result<ChatStream> {
                let chunk = ChatChunk {
                    delta: "hello".into(),
                    usage: Some(Usage {
                        input_tokens: 5,
                        output_tokens: 1,
                        cache_creation_input_tokens: 0,
                        cache_read_input_tokens: 0,
                        provider: "test",
                        model: "m".into(),
                    }),
                };
                Ok(Box::pin(futures::stream::iter(vec![Ok(chunk)])))
            }
            fn provider_name(&self) -> &'static str {
                "test"
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test-stream.jsonl");
        let inner = Arc::new(OneChunkWithUsage);
        let tb = TelemetryBackend::new(inner, "ask.generate").with_path_override(path.clone());

        use futures::StreamExt;
        let mut s = tb.generate_stream(req()).await.unwrap();
        while let Some(c) = s.next().await {
            let _ = c.unwrap();
        }
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.lines().count(), 1);
        let rec: LlmCallRecord = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert_eq!(rec.stage, "ask.generate");
        assert!(rec.stream);
        assert_eq!(rec.input_tokens, 5);
        assert_eq!(rec.output_tokens, 1);
    }

    #[tokio::test]
    async fn generate_records_failure_with_zero_tokens() {
        struct AlwaysFails;
        #[async_trait]
        impl ChatBackend for AlwaysFails {
            async fn generate(&self, _: ChatRequest<'_>) -> Result<ChatResponse> {
                anyhow::bail!("simulated error")
            }
            async fn generate_stream(&self, _: ChatRequest<'_>) -> Result<ChatStream> {
                anyhow::bail!("simulated error")
            }
            fn provider_name(&self) -> &'static str {
                "test"
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test-fail.jsonl");
        let inner = Arc::new(AlwaysFails);
        let tb = TelemetryBackend::new(inner, "rewriter").with_path_override(path.clone());

        let r = tb.generate(req()).await;
        assert!(r.is_err());

        let body = std::fs::read_to_string(&path).unwrap();
        let rec: LlmCallRecord = serde_json::from_str(body.lines().next().unwrap()).unwrap();
        assert!(!rec.success);
        assert_eq!(rec.input_tokens, 0);
        assert_eq!(rec.output_tokens, 0);
    }
}
