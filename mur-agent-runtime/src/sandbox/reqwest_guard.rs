//! Outbound HTTP host enforcement.
//!
//! - [`policy`]: [`HostGuard`], the DNS-resolver guard, and the URL / SSRF
//!   screens it is built from.
//! - [`client`]: [`GuardedHttpClient`], the client that cannot be used
//!   unguarded.

mod client;
mod policy;

pub use client::GuardedHttpClient;
pub(crate) use policy::screened_socket_addrs;
pub use policy::{HostGuard, check_request_url, host_allowed, host_matches_pattern};
