//! `/review` inside MURMUR (Phase 3b): parse the line, show the session
//! state, and run the review driver on a worker thread.

mod args;
mod confirm;
pub mod hint;
pub mod hitl;
pub mod keys;
mod render;
mod start;
mod state;

pub use confirm::{dismiss_prompt, on_finished, on_request};
pub use hitl::ReviewGate;
pub use render::{footer_label, footer_right_hint};
pub use state::handle;
pub use state::{ReviewSession, answer, esc_state, open_findings};

#[cfg(test)]
#[path = "args_tests.rs"]
mod args_tests;
#[cfg(test)]
#[path = "confirm_tests.rs"]
mod confirm_tests;
#[cfg(test)]
#[path = "footer_tests.rs"]
mod footer_tests;
#[cfg(test)]
#[path = "hint_tests.rs"]
mod hint_tests;
#[cfg(test)]
#[path = "hitl_tests.rs"]
mod hitl_tests;
#[cfg(test)]
#[path = "keys_tests.rs"]
mod keys_tests;
#[cfg(test)]
#[path = "render_tests.rs"]
mod render_tests;
#[cfg(test)]
#[path = "resume_tests.rs"]
mod resume_tests;
#[cfg(test)]
#[path = "ruling_tests.rs"]
mod ruling_tests;
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
