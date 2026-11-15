//! Filled in Task 4.

use std::path::{Path, PathBuf};

pub struct PendingRequest;
pub struct Written;

pub fn requests_dir(agent_home: &Path) -> PathBuf {
    agent_home.join(super::REQUESTS_DIR)
}
pub fn write() {}
pub fn read_pending() {}
pub fn remove() {}
