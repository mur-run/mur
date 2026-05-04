//! Track C2 — Telegram bridge runtime modules.
//!
//! The Telegram bridge is an LLM-less mur agent that long-polls
//! `getUpdates` against the Bot API and forwards inbound user messages
//! to the local A2A bus as `message/send` requests, signed by the
//! bridge's identity key (re-using the C1 plumbing under
//! [`crate::bridge::dedupe`], [`crate::bridge::ack`], and
//! [`mur_common::bridge::envelope`]).
//!
//! ## Sub-modules
//!
//! - [`inbound`] — long-poll loop ([`inbound::TelegramInboundLoop`])
//! - [`mock`] — `MockBot` test double + `MockUserAgent` for routing tests
//! - [`voice`] — voice-message handler (M-c2.3, stub)
//! - [`files`] — document/photo handler (M-c2.4, stub)
//! - [`mcp`] — Telegram-specific MCP tool surface (M-c2.5, stub)

pub mod files;
pub mod inbound;
pub mod mcp;
pub mod mock;
pub mod voice;
