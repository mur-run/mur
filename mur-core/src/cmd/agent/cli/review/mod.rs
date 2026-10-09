//! `/review` inside MURMUR (Phase 3b): parse the line, show the session
//! state, and run the review driver on a worker thread.

mod args;
mod confirm;
mod render;
mod start;
mod state;

pub use confirm::{answer_confirm, on_finished, on_request};
pub use start::answer_resume;
pub use state::handle;
pub use state::{ReviewSession, is_send_gate};

#[cfg(test)]
#[path = "args_tests.rs"]
mod args_tests;
#[cfg(test)]
#[path = "confirm_tests.rs"]
mod confirm_tests;
#[cfg(test)]
#[path = "resume_tests.rs"]
mod resume_tests;
#[cfg(test)]
#[path = "slash_tests.rs"]
mod slash_tests;
#[cfg(test)]
#[path = "start_tests.rs"]
mod start_tests;
#[cfg(test)]
#[path = "state_tests.rs"]
mod state_tests;
#[cfg(test)]
#[path = "test_fixtures.rs"]
mod test_fixtures;
