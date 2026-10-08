//! `/review` inside MURMUR (Phase 3b): parse the line, show the session
//! state, and run the review driver on a worker thread.

mod args;
mod render;
mod state;

pub use state::ReviewSession;
#[allow(unused_imports)] // wired in PR 3 (Task 7): slash.rs dispatches `/review` here
pub use state::handle;

#[cfg(test)]
#[path = "args_tests.rs"]
mod args_tests;
#[cfg(test)]
#[path = "state_tests.rs"]
mod state_tests;
