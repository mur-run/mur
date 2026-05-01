use serde::{Deserialize, Serialize};

/// One line in `<agent_dir>/telemetry/inputs.jsonl`.
///
/// Append-only; per-turn read by `B0SafetyHook::on_prompt_submit` to
/// know which untrusted artifacts to wrap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceEntry {
    pub sha256: String,
    /// e.g. `"user_drop"`, `"user_paste"`, `"a2a_attachment"`.
    pub source: String,
    pub decoder_version: String,
    pub ocr_engine_version: Option<String>,
    /// Monotonic per-agent turn counter.
    pub turn_id: u64,
    pub recorded_at: chrono::DateTime<chrono::Utc>,
}
