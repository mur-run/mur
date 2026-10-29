//! The orchestrator's audit lines, all to the agent's
//! `permissions/audit.jsonl`. A failed append is reported, never fatal: the
//! file operations have already happened (Hub spec §Audit).

use std::path::Path;

use chrono::Utc;
use mur_common::agent::OverrideFlags;
use mur_common::permissions::{AuditEvent, GrantStore};

pub(in crate::cmd::agent) fn append(home: &Path, name: &str, event: &AuditEvent) {
    let store = GrantStore::new(home.join("agents").join(name));
    if let Err(e) = store.append_audit(event) {
        eprintln!("warning: could not write the override audit line for '{name}': {e}");
    }
}

pub(in crate::cmd::agent) fn declined(home: &Path, name: &str, flags: OverrideFlags) {
    append(
        home,
        name,
        &AuditEvent::StartupOverrideDeclined {
            ts: Utc::now(),
            flags,
            window: None,
        },
    );
}
