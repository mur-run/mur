//! The MURMUR side of the review loop (P3b): the bridge types that carry a
//! worker thread's questions to the UI thread, and the transport that asks them.

pub mod bridge;
pub mod transport;

#[cfg(test)]
#[path = "transport_tests.rs"]
mod transport_tests;
