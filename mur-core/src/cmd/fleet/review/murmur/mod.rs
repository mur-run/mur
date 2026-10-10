//! The MURMUR side of the review loop (P3b): the bridge types that carry a
//! worker thread's questions to the UI thread, and the transport that asks them.

pub mod bridge;
pub mod transport;
pub mod worker;

#[cfg(test)]
#[path = "transport_tests.rs"]
mod transport_tests;

#[cfg(test)]
#[path = "worker_tests.rs"]
mod worker_tests;
