//! ssh-agent proxy core (plan Phase 2). Spec:
//! `docs/superpowers/specs/2026-10-14-ssh-agent-proxy-design.md`.
//!
//! The wire codec, `session-bind` verification, the key → host-key policy,
//! refusal reasons, the approval-gate seam ([`gate`]), and the per-connection
//! logic over an abstract upstream. The supervisor starts it in-process
//! around the seal (plan O7, option A; `supervisor/ssh_proxy_boot.rs`). A
//! signature is produced only for a request that passed every binding rule,
//! the policy, and the approval gate.

pub mod gate;
pub mod keys;
#[cfg(unix)]
pub mod listener;
pub mod policy;
pub mod refusal;
pub(crate) mod session;
pub(crate) mod userauth;
pub mod wire;

#[cfg(test)]
mod tests;
