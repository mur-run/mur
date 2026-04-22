//! Embedded agent assets (compile-time bundled via `build.rs`).
//!
//! With `--features=embedded-agent` and `MUR_EXPORT_AGENT_DIR` pointing at an
//! agent home at build time, the generated tar.gz is pulled in via
//! `include_bytes!` and exposed as `EMBEDDED_TAR`. Without the feature the
//! constant is absent and `has_embedded_agent()` returns false so the
//! supervisor falls back to MUR_HOME-based discovery (Task 35 diverts to the
//! extraction dir when an agent *is* embedded).

#[cfg(feature = "embedded-agent")]
pub const EMBEDDED_TAR: &[u8] = include_bytes!(env!("MUR_EMBEDDED_AGENT_PATH"));

/// Whether this binary carries an agent bundle.
pub fn has_embedded_agent() -> bool {
    #[cfg(feature = "embedded-agent")]
    {
        !EMBEDDED_TAR.is_empty()
    }
    #[cfg(not(feature = "embedded-agent"))]
    {
        false
    }
}
