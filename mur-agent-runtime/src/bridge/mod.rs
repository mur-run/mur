//! # Bridge support for the A2A runtime
//!
//! A "bridge" is a small, LLM-less mur agent that ferries messages between
//! a chat platform and a user agent. Envelope verification runs **regardless
//! of transport** (Unix socket has no peer auth; Noise XK only proves *some*
//! peer's identity, not authorization to claim the bridge role).
//!
//! ## Trust model
//!
//! Verification ([`verify::verify_inbound_envelope`]) runs **regardless of
//! transport**. Every inbound A2A envelope from a bridge MUST carry a
//! `SignedEnvelope` and the user agent MUST verify against
//! `profile.yaml.trusted_peers[]` before processing.
//!
//! ## Sub-modules
//!
//! - [`beacon`] — 30 s `telemetry/bridge_alive` heartbeat + degraded classifier
//! - [`dedupe`] — sled-backed `(bridge_id, platform_msg_id)`, 7-day TTL
//! - [`verify`] — envelope verification against a trust list

pub mod beacon;
pub mod dedupe;
pub mod verify;
