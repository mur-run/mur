// MUR Core v2 — capture module
//
// The learning pipeline: noise filter → significance → extractor → dedup → verify → link

pub mod curator;
pub mod emergence;
pub mod feedback;
pub mod import;
pub mod noise_filter;
pub mod reflector;
pub mod starter;

pub use curator::curate;
pub use reflector::reflect_session;
