//! Git push broker. Unix-only: the parser sandbox relies on `setrlimit`, `pre_exec`
//! and POSIX permission bits, none of which exist on Windows. The crate compiles
//! to an empty library there so `cargo --workspace` keeps working.
#![cfg(unix)]

pub mod action;
pub mod ancestry;
pub mod approval;
pub mod broker;
pub mod constants;
pub mod error;
pub mod git;
pub mod import;
pub mod oid;
pub mod pending;
pub mod policy;
pub mod prefetch;
pub mod push;
pub mod repo;
