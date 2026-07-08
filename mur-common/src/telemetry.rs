//! OpenTelemetry GenAI semantic conventions (Q1 2026 Development status,
//! gated by `OTEL_SEMCONV_STABILITY_OPT_IN=gen_ai_latest_experimental`)
//! plus murmur-specific extensions.
//!
//! Sensitive payloads (`gen_ai.input.messages`, `output.messages`,
//! `tool.call.arguments`, `tool.call.result`) are opt-in via
//! `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=true`. mur's
//! existing redaction modes (full / redacted / metadata-only) align.

// ───── gen_ai.* core (Development) ─────
// `gen_ai.system` was deprecated in favour of `gen_ai.provider.name` (2025).
pub const GEN_AI_PROVIDER_NAME: &str = "gen_ai.provider.name";
pub const GEN_AI_OPERATION_NAME: &str = "gen_ai.operation.name";
pub const GEN_AI_REQUEST_MODEL: &str = "gen_ai.request.model";
pub const GEN_AI_RESPONSE_MODEL: &str = "gen_ai.response.model";
pub const GEN_AI_RESPONSE_FINISH_REASONS: &str = "gen_ai.response.finish_reasons";
pub const GEN_AI_USAGE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";
pub const GEN_AI_USAGE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";

// ───── gen_ai.agent.* + conversation correlation ─────
pub const GEN_AI_AGENT_ID: &str = "gen_ai.agent.id";
pub const GEN_AI_AGENT_NAME: &str = "gen_ai.agent.name";
pub const GEN_AI_CONVERSATION_ID: &str = "gen_ai.conversation.id";

// ───── gen_ai.tool.* ─────
pub const GEN_AI_TOOL_NAME: &str = "gen_ai.tool.name";
pub const GEN_AI_TOOL_TYPE: &str = "gen_ai.tool.type";
pub const GEN_AI_TOOL_CALL_ID: &str = "gen_ai.tool.call.id";

// ───── error / mcp / network (Stable spec) ─────
pub const ERROR_TYPE: &str = "error.type";
pub const MCP_METHOD_NAME: &str = "mcp.method.name";
pub const MCP_SESSION_ID: &str = "mcp.session.id";
pub const NETWORK_TRANSPORT: &str = "network.transport";

// ───── mur.* (no spec coverage; preserve) ─────
pub const MUR_AGENT_UUID: &str = "mur.agent.uuid";
pub const MUR_AGENT_NAME: &str = "mur.agent.name";
pub const MUR_TASK_ID: &str = "mur.task.id";
pub const MUR_MCP_SERVER: &str = "mur.mcp.server";
pub const MUR_ENTITLEMENT_DENIED: &str = "mur.entitlement.denied"; // P0b usage
pub const MUR_COST_USD: &str = "mur.cost_usd";
pub const MUR_TRIGGER_KIND: &str = "mur.trigger.kind";
pub const MUR_A2A_PEER_PUBKEY: &str = "mur.a2a.peer.pubkey";
pub const MUR_HOOK_NAME: &str = "mur.hook.name";
pub const MUR_HOOK_PHASE: &str = "mur.hook.phase";
pub const MUR_FIRED_SKILLS: &str = "mur.fired_skills";
pub const MUR_EVENT_TYPE: &str = "mur.event.type";

// ───── method names emitted on the JSON-RPC notification side-channel ─────
pub const METHOD_LLM_CALL: &str = "telemetry/llm_call";
pub const METHOD_TOOL_CALL: &str = "telemetry/tool_call";
pub const METHOD_ERROR: &str = "telemetry/error";
pub const METHOD_HEARTBEAT: &str = "telemetry/heartbeat";
pub const METHOD_WARNING: &str = "telemetry/warning";
pub const METHOD_TASK_PROGRESS: &str = "task/progress";
pub const METHOD_HOOK_FIRED: &str = "telemetry/hook_fired";
pub const METHOD_BRIDGE_ALIVE: &str = "telemetry/bridge_alive";
pub const METHOD_SKILL_EXECUTED: &str = "mur.skill.executed";
pub const METHOD_SKILL_INDEXED: &str = "mur.skill.indexed";
pub const METHOD_SKILL_STEP_RESOLVED: &str = "mur.skill.step_resolved";

/// A `category: note` skill was surfaced to the user by a query. Counted by
/// `reindex_stats` as a usage + success so retrieval drives the note lifecycle.
pub const METHOD_NOTE_RETRIEVED: &str = "mur.note.retrieved";

/// Emitted by `mur skill curate` — a human reviewed an LLM-extracted skill.
/// Reduced into `SkillStats::curated_at`; opens the A1 provenance gate.
pub const METHOD_SKILL_CURATED: &str = "mur.skill.curated";

/// Emitted by `mur agent mcp set-network --broad-audited` — an operator
/// granted a per-server BroadAudited egress policy (advisory-enforced,
/// audited-CONNECT). Records which agent+server was authorized.
pub const METHOD_EGRESS_BROAD_AUDITED_ENABLED: &str = "mur.egress.broad_audited.enabled";

pub const MUR_SKILL_NAME: &str = "mur.skill.name";
pub const MUR_SKILL_VERSION: &str = "mur.skill.version";
pub const MUR_SKILL_OUTCOME: &str = "mur.skill.outcome";
pub const MUR_SKILL_DURATION_MS: &str = "mur.skill.duration_ms";
pub const MUR_SKILL_MANIFEST_DIGEST: &str = "mur.skill.manifest_digest";
