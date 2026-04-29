//! Permission grants and audit log for the AskUser flow.
//!
//! `Decision::AskUser` from a `pre_tool_use` hook surfaces an inline
//! approval card to the user; the GUI writes the user's choice into a
//! `Grant` here. Grants are scoped by `(agent_id, tool_name,
//! sha256(canonical input subset))` — each tool declares which input
//! fields contribute (e.g. bash → argv[0]; fs.write → directory prefix).
//!
//! Storage:
//! * `~/.mur/agents/<name>/permissions/grants.yaml` (0600, atomic
//!   temp+rename)
//! * `~/.mur/agents/<name>/permissions/audit.jsonl` (append-only,
//!   never mutated)
//!
//! The `B0SafetyHook` (M8) reads grants in `pre_tool_use`; the GUI
//! Settings → Permissions tab manages them (revoke / list).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ScopeKey {
    pub agent_id: String,
    pub tool_name: String,
    /// SHA-256 (hex) over the canonical-JSON of a per-tool subset of inputs.
    pub input_schema_hash: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum GrantDecision {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum GrantSource {
    Ui,
    Headless,
    Default,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    pub scope_key: ScopeKey,
    pub decision: GrantDecision,
    pub granted_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub source: GrantSource,
    pub source_audit_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuditEvent {
    AskedUser {
        ts: chrono::DateTime<chrono::Utc>,
        scope_key: ScopeKey,
        prompt_hash: String,
        ttl_ms: u64,
    },
    GrantWritten {
        ts: chrono::DateTime<chrono::Utc>,
        scope_key: ScopeKey,
        decision: GrantDecision,
        source: GrantSource,
    },
    GrantUsed {
        ts: chrono::DateTime<chrono::Utc>,
        scope_key: ScopeKey,
    },
    HeadlessDenied {
        ts: chrono::DateTime<chrono::Utc>,
        scope_key: ScopeKey,
    },
    Revoked {
        ts: chrono::DateTime<chrono::Utc>,
        scope_key: ScopeKey,
    },
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct GrantsFile {
    pub version: u32,
    pub grants: Vec<Grant>,
}

/// Read/write `~/.mur/agents/<name>/permissions/grants.yaml` (0600,
/// atomic temp+rename) plus an append-only `audit.jsonl`.
pub struct GrantStore {
    grants_path: PathBuf,
    audit_path: PathBuf,
    cache: HashMap<ScopeKey, Grant>,
}

impl GrantStore {
    /// `agent_dir` is `~/.mur/agents/<name>/`; this stores under
    /// `<agent_dir>/permissions/`.
    pub fn new<P: AsRef<Path>>(agent_dir: P) -> Self {
        let dir = agent_dir.as_ref().join("permissions");
        Self {
            grants_path: dir.join("grants.yaml"),
            audit_path: dir.join("audit.jsonl"),
            cache: HashMap::new(),
        }
    }

    pub fn load(&mut self) -> std::io::Result<()> {
        if !self.grants_path.exists() {
            return Ok(());
        }
        let bytes = std::fs::read(&self.grants_path)?;
        let file: GrantsFile = serde_yaml_ng::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.cache.clear();
        for g in file.grants {
            self.cache.insert(g.scope_key.clone(), g);
        }
        Ok(())
    }

    /// Returns the live `Decision` if the grant is present and not
    /// expired; `None` otherwise (caller must AskUser or auto-Deny).
    pub fn lookup(
        &self,
        key: &ScopeKey,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Option<GrantDecision> {
        self.cache.get(key).and_then(|g| {
            if let Some(expires) = g.expires_at
                && now > expires
            {
                return None;
            }
            Some(g.decision)
        })
    }

    pub fn insert(&mut self, grant: Grant) -> std::io::Result<()> {
        self.cache.insert(grant.scope_key.clone(), grant);
        self.persist()
    }

    pub fn revoke(
        &mut self,
        key: &ScopeKey,
        now: chrono::DateTime<chrono::Utc>,
    ) -> std::io::Result<()> {
        if self.cache.remove(key).is_none() {
            return Ok(()); // idempotent
        }
        self.append_audit(&AuditEvent::Revoked {
            ts: now,
            scope_key: key.clone(),
        })?;
        self.persist()
    }

    pub fn append_audit(&self, event: &AuditEvent) -> std::io::Result<()> {
        if let Some(parent) = self.audit_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut line = serde_json::to_string(event)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.audit_path)?;
        f.write_all(line.as_bytes())?;
        Ok(())
    }

    fn persist(&self) -> std::io::Result<()> {
        if let Some(parent) = self.grants_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = GrantsFile {
            version: 1,
            grants: self.cache.values().cloned().collect(),
        };
        let bytes = serde_yaml_ng::to_string(&file)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = self.grants_path.with_extension("yaml.tmp");
        std::fs::write(&tmp, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&tmp, perms)?;
        }
        std::fs::rename(&tmp, &self.grants_path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn key(tool: &str) -> ScopeKey {
        ScopeKey {
            agent_id: "coach".into(),
            tool_name: tool.into(),
            input_schema_hash: "sha".into(),
        }
    }

    #[test]
    fn insert_and_lookup_round_trip() {
        let dir = tempdir().unwrap();
        let mut store = GrantStore::new(dir.path());
        let now = chrono::Utc::now();
        let k = key("fs.write");
        store
            .insert(Grant {
                scope_key: k.clone(),
                decision: GrantDecision::Allow,
                granted_at: now,
                expires_at: Some(now + chrono::Duration::days(30)),
                last_used_at: None,
                source: GrantSource::Ui,
                source_audit_id: None,
            })
            .unwrap();

        let mut store2 = GrantStore::new(dir.path());
        store2.load().unwrap();
        assert_eq!(store2.lookup(&k, now), Some(GrantDecision::Allow));
    }

    #[test]
    fn lookup_returns_none_for_expired_grant() {
        let dir = tempdir().unwrap();
        let mut store = GrantStore::new(dir.path());
        let now = chrono::Utc::now();
        let k = key("fs.write");
        store
            .insert(Grant {
                scope_key: k.clone(),
                decision: GrantDecision::Allow,
                granted_at: now,
                expires_at: Some(now - chrono::Duration::seconds(1)),
                last_used_at: None,
                source: GrantSource::Ui,
                source_audit_id: None,
            })
            .unwrap();
        assert_eq!(store.lookup(&k, now), None);
    }

    #[test]
    fn audit_log_appends_and_persists() {
        let dir = tempdir().unwrap();
        let store = GrantStore::new(dir.path());
        let now = chrono::Utc::now();
        store
            .append_audit(&AuditEvent::AskedUser {
                ts: now,
                scope_key: key("bash"),
                prompt_hash: "abc".into(),
                ttl_ms: 120_000,
            })
            .unwrap();
        store
            .append_audit(&AuditEvent::HeadlessDenied {
                ts: now,
                scope_key: key("net.connect"),
            })
            .unwrap();

        let body = std::fs::read_to_string(dir.path().join("permissions/audit.jsonl")).unwrap();
        let lines: Vec<_> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("asked_user"));
        assert!(lines[1].contains("headless_denied"));
    }

    #[test]
    fn revoke_drops_grant_and_writes_audit() {
        let dir = tempdir().unwrap();
        let mut store = GrantStore::new(dir.path());
        let now = chrono::Utc::now();
        let k = key("fs.write");
        store
            .insert(Grant {
                scope_key: k.clone(),
                decision: GrantDecision::Allow,
                granted_at: now,
                expires_at: None,
                last_used_at: None,
                source: GrantSource::Ui,
                source_audit_id: None,
            })
            .unwrap();
        store.revoke(&k, now).unwrap();
        assert_eq!(store.lookup(&k, now), None);
        let body = std::fs::read_to_string(dir.path().join("permissions/audit.jsonl")).unwrap();
        assert!(body.contains("revoked"));
    }
}
