pub mod a2a;
pub mod actor;
pub mod agent;
pub mod agent_name;
pub mod bundle;
pub mod config;
pub mod conversation;
pub mod error;
pub mod event;
pub mod identity;
pub mod knowledge;
pub mod llm;
pub mod lock_file;
pub mod parameterize;
pub mod pattern;
pub mod pipeline;
pub mod schedule;
pub mod schedule_claim;
pub mod scope;
pub mod signal;
pub mod telemetry;
pub mod variable;
pub mod workflow;

pub use actor::{Actor, ActorSource};
pub use conversation::{CONVERSATION_SCHEMA_VERSION, Content, Message, Role, Source};
pub use pattern::Pattern;
pub use scope::Scope;
pub use signal::{SIGNAL_SCHEMA_VERSION, Signal, SignalKind, SignalTarget};

pub use a2a::Message as A2aMessage;
pub use a2a::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, Task, TaskState};
pub use agent::{
    AgentProfile, DeploymentConfig, DeploymentType, Entitlements, ExecutionMode,
    FileTransferConfig, IdentityConfig, LockFile, Persona, PersonaCategory, RetryPolicy,
    ScheduleEntry,
};
pub use agent_name::{AgentNameError, MAX_AGENT_NAME_LEN, validate_agent_name};
pub use identity::{
    AgentIdentity, ChainError, ChainOptions, ChainOutcome, IdentityError, RotationAttestation,
    RotationReason, decode_pubkey, encode_pubkey, verify_chain,
};
