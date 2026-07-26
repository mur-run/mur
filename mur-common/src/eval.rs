//! B0 M11 — eval-harness output schema.
//!
//! Per-test JSONL records emitted by the Python harness in
//! `scripts/eval/` and consumed by the Rust report aggregator
//! (`mur agent eval report`, M11.4). Stable on-disk shape — bumping
//! field semantics requires a new `EvalRecord.schema_version`.
//!
//! Spec: `docs/superpowers/specs/2026-05-06-b0-m11-eval-harness-design.md` §6.

use serde::{Deserialize, Serialize};

/// Schema version of the JSONL records this build emits / consumes.
/// Increment when the JSONL contract changes; the report aggregator
/// rejects records with a version it doesn't recognise.
pub const EVAL_SCHEMA_VERSION: u32 = 1;

/// Outcome the agent took in response to the attack — independent of
/// whether that outcome was the "right" one (which is up to the test
/// case's `expected` field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalDecision {
    /// Agent refused the attacker's instruction outright (a B0 hook
    /// returned `Decision::Deny` or the model declined to proceed).
    Refuse,
    /// Agent did the user's intended thing without falling for the
    /// embedded directive — the safe-and-correct outcome on most
    /// indirect-injection cases.
    ComplySafe,
    /// Agent followed the attacker's directive — TEST FAIL on every
    /// indirect-injection / jailbreak / agentic-misuse case.
    ComplyUnsafe,
}

/// Which upstream benchmark the case came from. Determines how the
/// aggregator buckets results in its markdown report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalSuite {
    /// Princeton/ETH/Anthropic AgentDojo (Apache-2.0, NeurIPS 2024).
    /// Indirect-prompt-injection focus.
    Agentdojo,
    /// CAIS HarmBench (CC-BY-4.0, ICML 2024). Jailbreak + agentic-
    /// misuse focus.
    Harmbench,
    /// UIUC/Kang-Lab InjecAgent (MIT). Tool-poisoning focus:
    /// 200 cases where injected tool outputs redirect the agent.
    #[serde(rename = "injecagent")]
    InjecAgent,
}

/// Which model backend produced the agent response. The mock backend
/// is the CI-track stub; everything else is a real-LLM release-track
/// run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalLlmBackend {
    /// Deterministic mock (`scripts/eval/mock_llm.py` — lands in M11.1).
    /// Returns canonical `refuse` / `comply_safe` / `comply_unsafe`
    /// per attack class. Fast, free, runs every PR.
    Stub,
    /// Anthropic API. Use for the canonical pass/fail vs. spec
    /// thresholds. Default model: claude-sonnet-5.
    Anthropic,
    /// OpenAI API.
    Openai,
    /// Local Ollama. Useful for gating without an API key but
    /// not the canonical baseline.
    Ollama,
    /// Llama-Guard-3-8B running locally via Ollama. Used as the
    /// nightly judge: grades each response as safe / unsafe.
    LlamaGuard,
}

/// One observation from the B0 hook chain during the test run.
/// Captured in chronological order so a later regression diagnosis
/// can replay the protection logic.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvalHookDecision {
    /// Hook name + phase, e.g. `"B0SafetyHook.on_prompt_submit"`.
    pub hook: String,
    /// Decision returned, e.g. `"wrap_untrusted"`, `"AskUser"`,
    /// `"Deny"`, `"Allow"`.
    pub decision: String,
    /// B0 rule number this firing maps to. None for hook decisions
    /// outside the rule numbering (e.g. provenance ledger writes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<u32>,
}

/// One test case's result — written as a single JSONL line by the
/// Python harness per case, parsed by `mur agent eval report` to
/// build the markdown summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvalRecord {
    /// Wire-version of this struct; mismatches abort report
    /// generation rather than producing silently-wrong aggregates.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub test_suite: EvalSuite,
    /// Stable identifier — `<suite>:<env>:<id>` for AgentDojo,
    /// `<suite>:<behavior_id>` for HarmBench.
    pub test_id: String,
    /// Free-form upstream tag, e.g. `"data_exfil"`,
    /// `"prompt_injection"`, `"agentic_misuse"`. Used to bucket
    /// the markdown report by category.
    pub attack_category: String,
    pub agent_decision: EvalDecision,
    pub expected: EvalDecision,
    pub passed: bool,
    /// B0 hook chain trace for this test, in order. Empty if the
    /// Python harness ran in fast-only mode (no hook capture).
    #[serde(default)]
    pub hook_decisions: Vec<EvalHookDecision>,
    /// LLM token usage. None for the stub backend (no real tokens).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_input: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_output: Option<u64>,
    pub wall_clock_ms: u64,
    pub llm_backend: EvalLlmBackend,
    /// Free-form model identifier — `"claude-sonnet-5"`,
    /// `"stub"`, `"llama3.2:3b"`, etc.
    pub llm_model: String,
    /// Run-id this record belongs to; aggregator groups by this.
    /// Format: ULID so records sort by time.
    pub run_id: String,
    /// RFC3339 timestamp of when this case finished.
    pub timestamp: String,
}

fn default_schema_version() -> u32 {
    EVAL_SCHEMA_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    /// JSONL contract: every record round-trips through serde_json
    /// without losing any field. Catches accidental schema drift.
    #[test]
    fn record_roundtrips_full_fields() {
        let r = EvalRecord {
            schema_version: EVAL_SCHEMA_VERSION,
            test_suite: EvalSuite::Agentdojo,
            test_id: "agentdojo:slack:42".into(),
            attack_category: "data_exfil".into(),
            agent_decision: EvalDecision::Refuse,
            expected: EvalDecision::Refuse,
            passed: true,
            hook_decisions: vec![
                EvalHookDecision {
                    hook: "B0SafetyHook.on_prompt_submit".into(),
                    decision: "wrap_untrusted".into(),
                    rule: Some(3),
                },
                EvalHookDecision {
                    hook: "B0SafetyHook.pre_tool_use".into(),
                    decision: "AskUser".into(),
                    rule: Some(4),
                },
            ],
            tokens_input: Some(9821),
            tokens_output: Some(184),
            wall_clock_ms: 1240,
            llm_backend: EvalLlmBackend::Anthropic,
            llm_model: "claude-sonnet-5".into(),
            run_id: "01HF8K0M5ZQEJ8C7XV6NQAYWZP".into(),
            timestamp: "2026-05-06T08:15:32.123Z".into(),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: EvalRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    /// Stub-backend records may omit `tokens_input` / `tokens_output`
    /// (no real LLM call → no tokens). The schema must accept the
    /// absent form on the way in.
    #[test]
    fn stub_record_without_tokens_roundtrips() {
        let yaml = r#"{
            "test_suite": "harmbench",
            "test_id": "harmbench:b042",
            "attack_category": "direct_jailbreak",
            "agent_decision": "refuse",
            "expected": "refuse",
            "passed": true,
            "wall_clock_ms": 38,
            "llm_backend": "stub",
            "llm_model": "stub",
            "run_id": "01HF8K0M5ZQEJ8C7XV6NQAYWZP",
            "timestamp": "2026-05-06T08:15:32.123Z"
        }"#;
        let r: EvalRecord = serde_json::from_str(yaml).unwrap();
        assert_eq!(r.schema_version, EVAL_SCHEMA_VERSION); // applied default
        assert_eq!(r.tokens_input, None);
        assert_eq!(r.tokens_output, None);
        assert!(r.hook_decisions.is_empty());
    }

    /// Decision serialization is snake_case so the Python harness
    /// can emit canonical strings without a Rust import.
    #[test]
    fn decision_strings_are_snake_case() {
        let r = serde_json::to_string(&EvalDecision::ComplyUnsafe).unwrap();
        assert_eq!(r, "\"comply_unsafe\"");
        let r = serde_json::to_string(&EvalSuite::Agentdojo).unwrap();
        assert_eq!(r, "\"agentdojo\"");
        let r = serde_json::to_string(&EvalLlmBackend::Anthropic).unwrap();
        assert_eq!(r, "\"anthropic\"");
    }

    /// Schema-version mismatch must be detectable without panic so
    /// the report aggregator can refuse to produce a misleading
    /// summary on a future-format JSONL.
    #[test]
    fn schema_version_constant_is_one() {
        assert_eq!(EVAL_SCHEMA_VERSION, 1);
    }

    #[test]
    fn injecagent_suite_roundtrips() {
        let s = serde_json::to_string(&EvalSuite::InjecAgent).unwrap();
        assert_eq!(s, "\"injecagent\"");
        let back: EvalSuite = serde_json::from_str(&s).unwrap();
        assert_eq!(back, EvalSuite::InjecAgent);
    }

    #[test]
    fn llama_guard_backend_roundtrips() {
        let s = serde_json::to_string(&EvalLlmBackend::LlamaGuard).unwrap();
        assert_eq!(s, "\"llama_guard\"");
        let back: EvalLlmBackend = serde_json::from_str(&s).unwrap();
        assert_eq!(back, EvalLlmBackend::LlamaGuard);
    }
}
