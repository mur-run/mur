//! Durable-monitor adapters and the daemon-facing service. The engine
//! itself is `mur-monitor`; this module is what needs `mur-core` (run
//! records) or a network client, and what assembles the registry.

pub mod actions;
pub mod adapters;
mod drain_actions;
pub mod notify;
pub mod service;

use std::path::Path;

use mur_monitor::adapter::AdapterRegistry;
use mur_monitor::spec::SourceType;

/// Every adapter this build ships. Task 10 and 11 add theirs here.
pub fn registry(mur_home: &Path) -> AdapterRegistry {
    let mut r = AdapterRegistry::new();
    r.register(Box::new(adapters::mur_run::MurRunAdapter::new(mur_home)));
    r.register(Box::new(
        adapters::github_actions::GithubActionsAdapter::default(),
    ));
    r.register(Box::new(adapters::subprocess::SubprocessAdapter::new(
        mur_home,
        SourceType::Codex,
    )));
    r.register(Box::new(adapters::subprocess::SubprocessAdapter::new(
        mur_home,
        SourceType::ClaudeCode,
    )));
    r
}
