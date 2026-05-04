//! Bridge-related shared types.
//!
//! A "bridge" is an LLM-less mur agent that relays messages between an
//! external chat platform (Slack, Discord, Telegram, …) and the A2A bus. The
//! types here describe schema bits shared across crates so bridges are a
//! first-class profile shape rather than an ad-hoc convention.

pub mod envelope;
pub mod llm_entitlement;
pub mod peer;
pub mod routes;
pub mod telegram_config;
pub use envelope::SignedEnvelope;
pub use llm_entitlement::{LlmEntitlement, LlmMode};
pub use peer::TrustedPeer;
pub use routes::{BridgeRouteConfig, InboundMessage, Resolution, RouteEntry, RouteMatch};
pub use telegram_config::{PrivacyMode, TelegramConfig};
