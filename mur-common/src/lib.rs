pub mod a2a;
pub mod actor;
pub mod agent;
pub mod agent_name;
pub mod bridge;
pub mod bundle;
pub mod canonical;
pub mod companion;
pub mod config;
pub mod conversation;
pub mod error;
/// B0 M11 — JSONL output schema for the eval harness.
pub mod eval;
pub mod event;
pub mod hooks_config;
pub mod hub;
pub mod identity;
pub mod knowledge;
pub mod llm;
pub mod lock_file;
pub mod model;
pub mod multimodal;
pub mod parameterize;
pub mod pattern;
pub mod permissions;
pub mod pipeline;
pub mod schedule;
pub mod schedule_claim;
pub mod scope;
pub mod secret;
pub mod signal;
pub mod telemetry;
pub mod variable;
pub mod workflow;

pub use actor::{Actor, ActorSource};
pub use conversation::{CONVERSATION_SCHEMA_VERSION, Content, Message, Role, Source};
pub use hooks_config::HooksConfig;
pub use pattern::Pattern;
pub use scope::Scope;
pub use signal::{SIGNAL_SCHEMA_VERSION, Signal, SignalKind, SignalTarget};

pub use a2a::Message as A2aMessage;
pub use a2a::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, Task, TaskState};
pub use agent::{
    AgentAppearance, AgentProfile, BehaviorPreset, DeploymentConfig, DeploymentType, Entitlements,
    ExecutionMode, FileTransferConfig, IdentityConfig, LockFile, Persona, PersonaCategory,
    RenderStatus, RetryPolicy, ScheduleEntry,
};
pub use agent_name::{AgentNameError, MAX_AGENT_NAME_LEN, validate_agent_name};
pub use bridge::{LlmEntitlement, LlmMode};
pub use identity::{
    AgentIdentity, ChainError, ChainOptions, ChainOutcome, IdentityError, RotationAttestation,
    RotationReason, decode_pubkey, encode_pubkey, verify_chain,
};
