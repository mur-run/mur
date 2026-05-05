//! Transport layer.
pub mod stdio;

#[cfg(unix)]
pub mod unix_socket;

pub mod noise;

pub mod tcp;

/// Track C5 / M5.2 — HTTP webhook receiver.
pub mod webhook;
