//! OpenTelemetry GenAI + murmur-specific field constants.
//! See spec §8.6 for the emitted notification shape.

pub const GEN_AI_SYSTEM: &str = "gen_ai.system";
pub const GEN_AI_REQUEST_MODEL: &str = "gen_ai.request.model";
pub const GEN_AI_USAGE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";
pub const GEN_AI_USAGE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";

pub const MUR_AGENT_UUID: &str = "mur.agent.uuid";
pub const MUR_AGENT_NAME: &str = "mur.agent.name";
pub const MUR_TASK_ID: &str = "mur.task.id";
pub const MUR_MCP_SERVER: &str = "mur.mcp.server";
pub const MUR_ENTITLEMENT_DENIED: &str = "mur.entitlement.denied";  // P0b usage

pub const METHOD_LLM_CALL: &str = "telemetry/llm_call";
pub const METHOD_TOOL_CALL: &str = "telemetry/tool_call";
pub const METHOD_ERROR: &str = "telemetry/error";
pub const METHOD_HEARTBEAT: &str = "telemetry/heartbeat";
pub const METHOD_WARNING: &str = "telemetry/warning";
pub const METHOD_TASK_PROGRESS: &str = "task/progress";
