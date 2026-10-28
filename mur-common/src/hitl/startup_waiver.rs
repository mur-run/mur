//! `--no-hitl`: a one-boot waiver for asks that came only from a default or a
//! clamp at a waivable tier. Not a wider grant ceiling — see
//! [`super::tier_may_be_waived`].

#[cfg(test)]
#[path = "startup_waiver_tests.rs"]
mod tests;
