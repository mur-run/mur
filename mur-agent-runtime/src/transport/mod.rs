//! Transport layer.
pub mod stdio;

#[cfg(unix)]
pub mod unix_socket;
