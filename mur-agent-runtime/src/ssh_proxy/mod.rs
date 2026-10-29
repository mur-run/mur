//! ssh-agent proxy core (plan Phase 2). Spec:
//! `docs/superpowers/specs/2026-10-14-ssh-agent-proxy-design.md`.
//!
//! What is here is placement-independent: the wire codec, `session-bind`
//! verification, the key → host-key policy, refusal reasons, and the
//! per-connection logic over an abstract upstream. Which process listens and
//! how it reaches the upstream past the agent's own sandbox is spec open item
//! "Proxy process placement" (plan O7); until that lands nothing here is
//! started, and no code path in this module ever produces a signature.

pub mod keys;
pub mod policy;
pub mod refusal;
pub(crate) mod session;
pub mod wire;

#[cfg(test)]
mod tests;
