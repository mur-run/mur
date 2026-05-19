// MUR Core v2 — capture module
//
// The learning pipeline: noise filter → significance → extractor → dedup → verify → link

pub mod emergence;
pub mod feedback;
pub mod import;
pub mod noise_filter;
pub mod reflector;
pub mod starter;

#[allow(unused_imports)]
pub use reflector::{ReflectResult, reflect_session};
// pub mod style; // removed: entire module was dead code (never called from main)
