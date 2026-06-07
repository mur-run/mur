//! Error type surfaced across the UniFFI boundary.

/// Errors returned by fallible SDK calls (today: the constructor).
///
/// Transport / connection failures are *not* returned synchronously — they
/// arrive asynchronously as [`crate::MobileEvent::Error`] / `Disconnected`
/// through the event listener, because connection work happens on the SDK's
/// internal runtime after the call returns.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum SdkError {
    /// The on-device identity keypair could not be loaded or created.
    #[error("identity: {msg}")]
    Identity { msg: String },

    /// The supplied configuration was invalid (e.g. unusable home path).
    #[error("config: {msg}")]
    Config { msg: String },

    /// The async runtime backing the client could not be started.
    #[error("runtime: {msg}")]
    Runtime { msg: String },
}
