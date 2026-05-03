//! # Bridge support for the A2A runtime
//!
//! A "bridge" is a small, LLM-less mur agent that ferries messages between
//! a chat platform and a user agent. Envelope verification runs **regardless
//! of transport** (Unix socket has no peer auth; Noise XK only proves *some*
//! peer's identity, not authorization to claim the bridge role).

pub mod dedupe;
pub mod verify;
