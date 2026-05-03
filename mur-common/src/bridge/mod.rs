//! Bridge-related shared types.
//!
//! A "bridge" is an LLM-less mur agent that relays messages between an
//! external chat platform (Slack, Discord, Telegram, …) and the A2A bus. The
//! types here describe schema bits shared across crates so bridges are a
//! first-class profile shape rather than an ad-hoc convention.

pub mod llm_entitlement;
pub mod routes;
pub use llm_entitlement::{LlmEntitlement, LlmMode};
pub use routes::{BridgeRouteConfig, InboundMessage, Resolution, RouteEntry, RouteMatch};
