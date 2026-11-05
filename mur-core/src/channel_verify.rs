//! Per-actor verify-on-fold (v3d-2). The verification itself lives in
//! [`mur_channel::verify`], below both `mur-core` and `mur-agent-runtime`, so
//! the runtime can verify an event without depending on `mur-core`.
pub(crate) use mur_channel::verify::actor_key_dir;
pub use mur_channel::verify::{actor_pubkey, verify_event};

/// Parse `MUR_CHANNEL_REQUIRE_SIG`: only explicit truthy values enable
/// signature enforcement (`=0` / `=false`, or unset, must NOT turn it on).
/// Shared by every reader of the var so the parsing rule lives in one place.
pub(crate) fn require_sig_from_env() -> bool {
    std::env::var("MUR_CHANNEL_REQUIRE_SIG")
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes"))
        .unwrap_or(false)
}
