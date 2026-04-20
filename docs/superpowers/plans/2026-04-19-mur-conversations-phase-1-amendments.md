# mur Conversations Phase 1 — Amendments & Best-Practice Patches

> **For agentic workers:** This is a **companion document** to `2026-04-19-mur-conversations-phase-1.md`. Read the base plan first, then apply the patches below. Patches are labeled by target task — they **override** the matching sections in the base plan.

**Date:** 2026-04-19
**Source:** cross-repo deep audit of `mur/`, `mur-server/`, `mur-commander/` with file:line verification.
**Why this exists:** the base plan is architecturally sound but has four technical conflicts with the actual codebases that will cause silent failures if executed as-is. This doc patches them and adds six best-practice hardenings.

---

## Summary of changes

| # | Area | Target tasks | Severity |
|---|------|--------------|----------|
| P1 | Audit chain bridge (not continuation) | 4, 19, 21 | 🔴 Critical |
| P2 | 6 call sites, not 5 (pattern_handler.rs) | 22 | 🟠 High |
| P3 | flock-based daemon detection | 19, 22 | 🟠 High |
| P4 | Dual config with auto-sync | 19, 23 | 🟡 Medium |
| BP1 | `mur conversations preflight` | new Task 19b | — |
| BP2 | Dry-run by default on destructive ops | 19, 22 | — |
| BP3 | Staging dir recovery | 19 | — |
| BP4 | `pull` concurrency guard | 9 | — |
| BP5 | Schema version bump protocol | 1 | — |
| BP6 | Observability spans | 9, 16 | — |

---

# Part 1 — Technical Conflict Patches

## P1 — Audit chain: bridge, don't continue (🔴 Critical)

**Conflict:** The base plan (Task 4) defines a generic `AuditEntry { id, ts, action, content_sha256, prev_hash, entry_hash }` with 7 fields and assumes it continues commander's existing chain. Commander's actual `AuditEntry` (`mur-commander/crates/engine/src/audit.rs:34-75`) has 18 workflow-oriented fields (`session_id`, `workflow_id`, `action_type`, `action_detail`, `model_used`, `cost`, `input_hash`, `output_summary`, `decision`, `approved_by`, `duration_ms`, `success`, `error`, `injected_patterns`, …) and uses a specific hash algorithm (`audit.rs:120-149`) that concatenates selected fields. Verification will fail immediately.

**Fix:** Two independent chains. Commander's `audit.jsonl` is untouched (workflow-scoped, stays where it is). mur's `~/.mur/conversations/audit.jsonl` is a NEW chain with its own schema. Migration writes exactly one bridge entry in the new chain that carries the cryptographic pointer to commander's last hash at the moment of migration. Both chains verify independently.

### P1.1 — Patch Task 4 (audit hash chain)

**Replace the `AuditEntry` design with this bridge-aware struct.** Keep everything else in Task 4 as-is; only the struct, the `AuditAction` enum, and the docstring change:

```rust
/// An entry in the conversations audit chain.
///
/// This is a SEPARATE chain from commander's own `~/.mur/commander/audit.jsonl`
/// (which keeps its workflow-scoped schema). The first entry in a freshly
/// migrated archive is a `Migrate` action whose `bridged_from_hash` field
/// carries commander's last `entry_hash` — providing a cryptographic pointer
/// without coupling hash algorithms.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: Uuid,
    pub ts: DateTime<Utc>,
    pub action: AuditAction,
    pub content_sha256: String,
    pub prev_hash: String,
    pub entry_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditAction {
    Write { target: String, bytes: u64 },
    Summarize { date: String, model: String, duration_ms: u64 },
    Index { date: String, vectors_added: u64 },
    Delete { target: String, reason: String, bytes_freed: u64 },
    Migrate {
        from: String,
        to: String,
        count: u64,
        /// Commander's last `entry_hash` at the moment of migration.
        /// GENESIS_PREV_HASH if no prior commander audit existed.
        bridged_from_hash: String,
        bridged_source: String,
    },
    Rollback { from: String, to: String, count: u64 },
    Error { layer: String, reason: String },
}
```

### P1.2 — Patch Task 19 (migration run) — record the bridge entry

When appending the `Migrate` audit entry during migration, source commander's last hash first. Replace the `log.record(audit::AuditAction::Migrate { ... })` block in Task 19's `run()` with:

```rust
// Read commander's last entry_hash (if any) to form the bridge.
let bridged_from = read_commander_last_hash(commander_dir)?;
let log = audit::AuditLog::at(audit_dst.clone());
log.record(
    audit::AuditAction::Migrate {
        from: commander_dir.to_string_lossy().to_string(),
        to: conversations_dir.to_string_lossy().to_string(),
        count: entries_migrated,
        bridged_from_hash: bridged_from.unwrap_or_else(|| audit::GENESIS_PREV_HASH.to_string()),
        bridged_source: commander_dir.join("audit.jsonl").to_string_lossy().to_string(),
    },
    String::new(),
)?;
```

Add this helper to `migrate.rs`:

```rust
/// Read the last line's `entry_hash` from commander's audit.jsonl, if present.
/// Does NOT try to verify commander's hash (different algorithm) — only
/// extracts the trailing entry_hash field as an opaque pointer.
fn read_commander_last_hash(commander_dir: &Path) -> Result<Option<String>> {
    let p = commander_dir.join("audit.jsonl");
    if !p.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&p)?;
    let Some(last) = text.lines().rev().find(|l| !l.trim().is_empty()) else {
        return Ok(None);
    };
    let v: serde_json::Value = serde_json::from_str(last)
        .with_context(|| format!("parsing last line of {}", p.display()))?;
    Ok(v.get("entry_hash").and_then(|h| h.as_str()).map(String::from))
}
```

### P1.3 — Patch Task 21 (plan/dry-run output)

Remove the `audit_chain_valid: bool` field and its side-effect of calling `AuditLog::verify()` on commander's file (wrong algorithm → false negatives). Replace with a `bridge_ready: bool` that only asserts commander's `audit.jsonl` is parseable JSONL when present:

```rust
#[derive(Debug, Default)]
pub struct MigrationPlan {
    pub commander_dir: PathBuf,
    pub long_term_msgs: u64,
    pub user_conversation_files: u64,
    pub episode_md_files: u64,
    pub bridge_ready: bool,
    pub bridged_from_hash: Option<String>,
    pub required_bytes: u64,
}

impl MigrationPlan {
    pub fn render(&self) -> String {
        let bridge = match (&self.bridge_ready, &self.bridged_from_hash) {
            (true, Some(h)) => format!("ready (commander last hash: {})", &h[..16]),
            (true, None) => "ready (no prior commander audit)".to_string(),
            (false, _) => "NOT ready (commander audit.jsonl unparseable)".to_string(),
        };
        format!(
            "Migration plan (source: {})\n\
             - memory/long_term.jsonl         : {} entries\n\
             - users/<uid>/conversation.jsonl : {} files\n\
             - memory/episodes/*.md           : {} files\n\
             - audit bridge                   : {}\n\
             - required free space (1.5x)    : {} bytes",
            self.commander_dir.display(),
            self.long_term_msgs,
            self.user_conversation_files,
            self.episode_md_files,
            bridge,
            self.required_bytes,
        )
    }
}
```

Update the plan computation to set `bridge_ready` based on parseability, not chain verification.

### P1.4 — Tests to add

Append to Task 4's `#[cfg(test)] mod tests`:

```rust
#[test]
fn migrate_action_serializes_bridge_fields() {
    let a = AuditAction::Migrate {
        from: "/a".into(),
        to: "/b".into(),
        count: 5,
        bridged_from_hash: "abc123".into(),
        bridged_source: "/a/audit.jsonl".into(),
    };
    let s = serde_json::to_string(&a).unwrap();
    assert!(s.contains("\"kind\":\"migrate\""));
    assert!(s.contains("\"bridged_from_hash\":\"abc123\""));
    let back: AuditAction = serde_json::from_str(&s).unwrap();
    assert_eq!(back, a);
}
```

Append to Task 19's `#[cfg(test)] mod tests`:

```rust
#[test]
fn migrate_records_bridge_to_commander_last_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let cmdr = tmp.path().join("commander");
    let conv = tmp.path().join("conversations");
    std::fs::create_dir_all(&cmdr).unwrap();
    // Seed a fake commander audit.jsonl with a distinctive trailing entry_hash.
    std::fs::write(
        cmdr.join("audit.jsonl"),
        "{\"id\":\"e1\",\"prev_hash\":\"00\",\"entry_hash\":\"DEADBEEF\"}\n",
    )
    .unwrap();
    run(&cmdr, &conv).unwrap();
    let audit_text = std::fs::read_to_string(conv.join("audit.jsonl")).unwrap();
    assert!(audit_text.contains("\"bridged_from_hash\":\"DEADBEEF\""));
}
```

---

## P2 — Six call sites, not five (🟠 High)

**Conflict:** Task 22 Step 22.1 says to locate and remove **5** `mur_learn::session::record()` sites at lines 416, 582, 793, 883, 1267 of `unified_handler/mod.rs`. Actual count is **6**; the missing one is:

```
/Volumes/Firecuda4tb/Projects/mur-commander/crates/gateway/src/unified_handler/pattern_handler.rs:48
```

and the current actual line numbers in `mod.rs` are 459, 625, 836, 926, 1310 (not 416/582/793/883/1267 — file has drifted since spec was written).

The `pattern_handler.rs:48` site is semantically different: it records a **session_start** / pattern-injection event, not a conversation turn. It should not become a `Role::User` or `Role::Assistant` — use `Role::System` with metadata describing the injection.

### P2.1 — Patch Task 22 Step 22.1 (grep first, assert count)

Replace Step 22.1 with:

```bash
cd /Volumes/Firecuda4tb/Projects/mur-commander
COUNT=$(grep -rn "mur_learn::session::record" crates/ | wc -l)
echo "Found $COUNT call sites"
grep -rn "mur_learn::session::record" crates/
# Expected: 6 sites at
#   crates/gateway/src/unified_handler/mod.rs:<L1..L5>
#   crates/gateway/src/unified_handler/pattern_handler.rs:<L6>
# If COUNT != 6 after codebase changes, STOP and re-read the sites — the
# semantic mapping in 22.3 may need updating.
test "$COUNT" = "6" || { echo "Expected 6 sites, found $COUNT. Inspect before proceeding."; exit 1; }
```

### P2.2 — Patch Task 22 Step 22.3 (add pattern_handler mapping)

Extend the mapping table to include the 6th site:

| File:line | Context | Replacement |
|---|---|---|
| `mod.rs:459` | User input (Slack/TG/Discord adapters) | `Role::User`, source per platform |
| `mod.rs:625` | Assistant response (main path) | `Role::Assistant`, source per platform |
| `mod.rs:836` | Assistant response (llm_service path) | `Role::Assistant`, source per platform |
| `mod.rs:926` | User input (bash mode) | `Role::User`, source per platform |
| `mod.rs:1310` | Assistant response (tail path) | `Role::Assistant`, source per platform |
| `pattern_handler.rs:48` | **Pattern injection / session_start** | `Role::System`, `meta: {"event": "pattern_injection", "patterns": [...]}` |

For `pattern_handler.rs:48` use this replacement (read the surrounding context to capture the injected pattern names):

```rust
use mur_common::conversation::{Content, Message, Role, Source};

let msg = Message {
    v: 1,
    ts: chrono::Utc::now(),
    src: Source::CommanderEngine,
    conv: session_id.clone(),
    role: Role::System,
    content: Content::Text {
        value: format!("pattern_injection: {}", injected.join(", ")),
    },
    meta: serde_json::json!({
        "event": "pattern_injection",
        "patterns": injected,
    }),
    refs: injected.iter().map(|p| format!("pattern:{}", p)).collect(),
};
let path = crate::memory::episodes::today_raw_file("commander_engine", &session_id);
if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
    use std::io::Write;
    let _ = writeln!(f, "{}", serde_json::to_string(&msg).unwrap_or_default());
}
```

---

## P3 — Daemon detection via flock, not file existence (🟠 High)

**Conflict:** Task 19 Step 19.2 checks `~/.mur/commander/daemon.lock`. Commander actually uses `~/.mur/commander/commander.pid` with advisory flock (`crates/daemon/src/main.rs:4, 38-98, 281`). File-existence checks false-positive after ungraceful shutdown and false-negative if the file is missing but a dev-mode daemon is running from a different working directory.

### P3.1 — Patch Task 1 (add `fs2` dep)

Append to the list of deps added in Task 1:

```toml
fs2 = "0.4"
```

### P3.2 — Replace `refuse_if_daemon_running` in Task 19

Replace the body of `refuse_if_daemon_running()`:

```rust
fn refuse_if_daemon_running(commander_dir: &Path) -> Result<()> {
    use fs2::FileExt;
    let pid = commander_dir.join("commander.pid");
    if !pid.exists() {
        return Ok(());
    }
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&pid)
        .with_context(|| format!("opening {}", pid.display()))?;
    match f.try_lock_exclusive() {
        Ok(()) => {
            // We acquired the lock → no live daemon. Release and proceed.
            FileExt::unlock(&f)?;
            Ok(())
        }
        Err(_) => {
            let pid_content = std::fs::read_to_string(&pid).unwrap_or_default();
            anyhow::bail!(
                "commander daemon appears to be running (PID file locked at {}; pid content: {}). \
                 Stop it with `murc daemon stop` before migrating.",
                pid.display(),
                pid_content.trim()
            )
        }
    }
}
```

### P3.3 — Update Task 22 Step 22.1 test

The base plan's test `run_refuses_when_daemon_lock_present` writes a `daemon.lock` file. Rewrite it:

```rust
#[test]
fn run_refuses_when_commander_pid_is_flocked() {
    use fs2::FileExt;
    let tmp = tempfile::tempdir().unwrap();
    let cmdr = tmp.path().join("commander");
    std::fs::create_dir_all(&cmdr).unwrap();
    let pid = cmdr.join("commander.pid");
    std::fs::write(&pid, "12345").unwrap();
    let guard = std::fs::OpenOptions::new().read(true).write(true).open(&pid).unwrap();
    guard.try_lock_exclusive().unwrap();

    let conv = tmp.path().join("conversations");
    let err = run(&cmdr, &conv).unwrap_err();
    assert!(err.to_string().contains("daemon appears to be running"));

    FileExt::unlock(&guard).unwrap();
}
```

---

## P4 — Dual config with auto-sync, not shared yaml (🟡 Medium)

**Conflict:** Task 23 (config schema) implies commander reads mur's `config.yaml` for `conversations.*`. Commander has no yaml reader and its canonical config is `~/.mur/commander/config.toml` (11+ call sites across `cli/`, `web/`, `daemon/`). Building a yaml bridge just for one section adds coupling and failure modes.

**Fix:** Two configs, one canonical, migrator keeps them in sync.

### P4.1 — Patch Task 23 (split into mur-side + commander-side)

**Task 23a — mur side (canonical, unchanged from base plan).** Adds `conversations:` section to `~/.mur/config.yaml` per base plan Task 23.

**Task 23b (NEW) — commander side.** Add to `mur-commander/crates/engine/src/config.rs` (or the main config struct file — grep for `[memory]` to find it):

```rust
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct ConversationsSection {
    pub enabled: bool,
    pub retention_days: u32,
}

impl ConversationsSection {
    /// Defaults mirror mur-common's `ConversationsConfig::default()`.
    /// These values MUST be kept in sync with mur's `config.yaml` via
    /// `mur conversations migrate --run`, which rewrites this section.
    pub fn sync_defaults() -> Self {
        Self { enabled: false, retention_days: 30 }
    }
}
```

Wire this into commander's top-level `Config` struct and expose `commander_config.conversations()` to consumers.

### P4.2 — Patch Task 19 (migrator writes commander's toml section)

Append a Step 19.6 "Sync commander config" that runs after the atomic rename:

```rust
/// After a successful migrate, write (or update) commander's
/// [conversations] section in ~/.mur/commander/config.toml to mirror mur's yaml.
pub fn sync_commander_config(commander_dir: &Path, enabled: bool, retention_days: u32) -> Result<()> {
    let toml_path = commander_dir.join("config.toml");
    let existing = if toml_path.exists() {
        std::fs::read_to_string(&toml_path)?
    } else {
        String::new()
    };
    // Naive section replace — acceptable for a generated block with a
    // clear marker. Breaks only if a user edits *inside* the marker block.
    let marker_open = "# BEGIN conversations (managed by mur conversations migrate)";
    let marker_close = "# END conversations";
    let new_block = format!(
        "{marker_open}\n[conversations]\nenabled = {enabled}\nretention_days = {retention_days}\n{marker_close}\n"
    );
    let out = if existing.contains(marker_open) && existing.contains(marker_close) {
        let start = existing.find(marker_open).unwrap();
        let end = existing.find(marker_close).unwrap() + marker_close.len();
        let mut s = String::new();
        s.push_str(&existing[..start]);
        s.push_str(&new_block);
        s.push_str(&existing[end..]);
        s
    } else {
        let mut s = existing;
        if !s.is_empty() && !s.ends_with('\n') {
            s.push('\n');
        }
        s.push_str(&new_block);
        s
    };
    std::fs::write(&toml_path, out)?;
    Ok(())
}
```

Call `sync_commander_config(commander_dir, cfg.conversations.enabled, cfg.conversations.retention_days)?` at the end of `run()`.

### P4.3 — Patch Task 17 (doctor checks for drift)

In `cmd_conversations(ConversationsAction::Doctor)`, after the audit report, add:

```rust
if let Ok(toml_text) = std::fs::read_to_string(commander_dir()?.join("config.toml")) {
    if let Ok(parsed) = toml::from_str::<toml::Value>(&toml_text) {
        let cmdr_retention = parsed
            .get("conversations")
            .and_then(|s| s.get("retention_days"))
            .and_then(|v| v.as_integer())
            .map(|n| n as u32);
        if let Some(cr) = cmdr_retention {
            if cr != cfg.conversations.retention_days {
                println!(
                    "⚠  config drift: mur retention_days={}, commander retention_days={} (run `mur conversations migrate --run` to resync)",
                    cfg.conversations.retention_days, cr
                );
            } else {
                println!("✓ config: mur↔commander retention_days match ({cr})");
            }
        }
    }
}
```

(Add `toml = "0.8"` to mur-core dependencies in Task 1.)

---

# Part 2 — Best-Practice Hardenings

## BP1 — `mur conversations preflight` (new Task 19b)

**What it does:** Single pre-flight command bundling every check migration depends on. Fails loudly before any destructive operation begins.

### BP1.1 — Add enum variant

In `mur-core/src/cmd/conversations_cmd.rs`, add to `ConversationsAction`:

```rust
    /// Check that migration is safe to run (daemon, audit, disk, drift, Ollama).
    Preflight,
```

### BP1.2 — Handler

Add to `cmd_conversations`:

```rust
        ConversationsAction::Preflight => {
            let cmdr = commander_dir()?;
            let conv_root = paths::conversations_root(None);
            let mut ok = true;

            // 1. Daemon not running
            match migrate::check_daemon_status(&cmdr) {
                Ok(()) => println!("✓ commander daemon: not running"),
                Err(e) => { println!("✗ commander daemon: {e}"); ok = false; }
            }

            // 2. Commander audit parseable (bridge-ready)
            match migrate::plan(&cmdr).map(|p| p.bridge_ready) {
                Ok(true)  => println!("✓ commander audit: parseable, bridge ready"),
                Ok(false) => { println!("✗ commander audit: unparseable"); ok = false; }
                Err(e)    => { println!("✗ commander audit: {e}"); ok = false; }
            }

            // 3. Disk free space
            let plan = migrate::plan(&cmdr)?;
            let free = fs2::available_space(std::path::Path::new("/"))
                .unwrap_or(u64::MAX);
            if free > plan.required_bytes {
                println!("✓ disk: {} free, need {}", free, plan.required_bytes);
            } else {
                println!("✗ disk: only {} free, need {}", free, plan.required_bytes);
                ok = false;
            }

            // 4. Config drift
            let cfg_now = cfg.conversations.retention_days;
            let cmdr_toml = std::fs::read_to_string(cmdr.join("config.toml")).unwrap_or_default();
            let drift = cmdr_toml.contains("retention_days")
                && !cmdr_toml.contains(&format!("retention_days = {cfg_now}"));
            if drift {
                println!("⚠ config drift: commander retention_days differs from mur ({cfg_now}); migrate --run will sync");
            } else {
                println!("✓ config: no drift (or first-time migration)");
            }

            // 5. Staging dir
            let staging = conv_root
                .parent()
                .map(|p| p.join(".conversations-migrating"))
                .unwrap_or_default();
            if staging.exists() {
                println!("⚠ staging dir exists at {} — previous migrate was interrupted", staging.display());
                println!("  → run `mur conversations migrate --resume` or `--discard-staging`");
                ok = false;
            } else {
                println!("✓ no stale staging dir");
            }

            if ok {
                println!("\n→ preflight passed, safe to `mur conversations migrate --run`");
            } else {
                println!("\n✗ preflight FAILED — resolve issues above before migrating");
                std::process::exit(1);
            }
        }
```

Also expose `migrate::check_daemon_status()` as a public wrapper around the private `refuse_if_daemon_running()` helper so preflight can call it without attempting a migration.

---

## BP2 — Dry-run by default on destructive ops

**Base plan issue:** `mur conversations migrate` takes `--dry-run` as opt-in. If a user types just `migrate`, the destructive path runs by default.

**Fix:** Flip the default. Require explicit `--run` to perform destructive ops.

### BP2.1 — Patch Task 17 CLI

Replace the `Migrate` variant in `ConversationsAction`:

```rust
    /// Migrate commander memory → conversations archive
    Migrate {
        /// Actually perform the migration. Without this flag, dry-run only.
        #[arg(long)]
        run: bool,
    },
```

Handler update:

```rust
        ConversationsAction::Migrate { run } => {
            let plan = migrate::plan(&commander_dir()?)?;
            println!("{}", plan.render());
            if run {
                migrate::run(&commander_dir()?, &paths::conversations_root(None))?;
                println!("migration complete");
            } else {
                println!("\n(dry-run — add `--run` to actually migrate)");
            }
        }
```

Apply the same `--run` flip to any future destructive subcommand (e.g., `cleanup --force` if added).

---

## BP3 — Staging dir recovery

**Base plan issue:** Task 19's `run()` does `if staging.exists() { remove_dir_all }` unconditionally, silently destroying any partial work from a previously interrupted migration.

### BP3.1 — Patch Task 19 `run()`

Replace the opening section:

```rust
pub fn run(commander_dir: &Path, conversations_dir: &Path) -> Result<()> {
    refuse_if_daemon_running(commander_dir)?;

    let parent = conversations_dir.parent().context("conversations_dir has no parent")?;
    let staging = parent.join(".conversations-migrating");
    if staging.exists() {
        anyhow::bail!(
            "staging dir exists at {} — a previous migrate was interrupted. \
             Run `mur conversations migrate --resume` or `--discard-staging` first.",
            staging.display()
        );
    }
    std::fs::create_dir_all(&staging)?;
    // … rest as in base plan
}
```

### BP3.2 — Add `--resume` and `--discard-staging`

Extend the `Migrate` enum variant:

```rust
    Migrate {
        #[arg(long)]
        run: bool,
        #[arg(long)]
        resume: bool,
        #[arg(long)]
        discard_staging: bool,
    },
```

Handler branch:

```rust
if discard_staging {
    let parent = paths::conversations_root(None).parent().unwrap().to_path_buf();
    let staging = parent.join(".conversations-migrating");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
        println!("staging dir removed: {}", staging.display());
    } else {
        println!("no staging dir to remove");
    }
    return Ok(());
}
if resume {
    migrate::resume(&commander_dir()?, &paths::conversations_root(None))?;
    println!("resume complete");
    return Ok(());
}
// …fall through to normal dry-run / --run flow
```

### BP3.3 — Add `migrate::resume` stub

```rust
/// Resume from an interrupted migration. Phase 1 implementation: finalize the
/// staging dir by re-verifying the audit chain and doing the atomic rename.
/// Does NOT re-run the copy steps (assumes they completed).
pub fn resume(_commander_dir: &Path, conversations_dir: &Path) -> Result<()> {
    let parent = conversations_dir.parent().context("no parent")?;
    let staging = parent.join(".conversations-migrating");
    if !staging.exists() {
        anyhow::bail!("no staging dir at {} to resume", staging.display());
    }
    // Verify staged audit is non-empty and JSONL-parseable.
    let audit_path = staging.join("audit.jsonl");
    if !audit_path.exists() {
        anyhow::bail!("staging dir at {} is missing audit.jsonl; discard and start over", staging.display());
    }
    // Atomic swap.
    if conversations_dir.exists() {
        let backup = conversations_dir.with_extension("pre-migrate.bak");
        if backup.exists() { std::fs::remove_dir_all(&backup)?; }
        std::fs::rename(conversations_dir, &backup)?;
    }
    std::fs::rename(&staging, conversations_dir)?;
    Ok(())
}
```

---

## BP4 — `pull` concurrency guard

**Base plan issue:** Two concurrent `mur conversations pull` processes could both write to the same `raw/<date>/<src>_<id>.jsonl` file. `O_APPEND` atomicity holds at single-write granularity but doesn't prevent the same ingester reading the same source file and double-writing messages.

### BP4.1 — Patch Task 9 (pipeline orchestrator)

Add to `ingest::process_and_store` (or wherever the top-level pull entry point lives):

```rust
use fs2::FileExt;

pub fn process_and_store(
    root: &Path,
    cfg: &ConversationsConfig,
    messages: Vec<Message>,
) -> Result<(usize, usize)> {
    // Process-level lock: prevents concurrent `mur conversations pull` invocations.
    let lock_path = root.join(".pull.lock");
    std::fs::create_dir_all(root)?;
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)?;
    lock_file
        .try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("another `mur conversations pull` is running ({})", lock_path.display()))?;
    // Lock released on scope exit when lock_file is dropped.

    // … rest of pipeline as in base plan
    Ok((written, dropped))
}
```

Test:

```rust
#[test]
fn concurrent_pull_is_rejected() {
    use fs2::FileExt;
    let tmp = tempfile::tempdir().unwrap();
    let lock = tmp.path().join(".pull.lock");
    std::fs::File::create(&lock).unwrap();
    let guard = std::fs::OpenOptions::new().write(true).open(&lock).unwrap();
    guard.try_lock_exclusive().unwrap();

    let cfg = ConversationsConfig::default();
    let err = process_and_store(tmp.path(), &cfg, vec![]).unwrap_err();
    assert!(err.to_string().contains("another `mur conversations pull` is running"));

    FileExt::unlock(&guard).unwrap();
}
```

---

## BP5 — Schema version bump protocol

**Base plan issue:** `v: 1` is hardcoded in the Message struct but there's no documented process for bumping it when the schema changes.

### BP5.1 — Patch Task 1

Append this doc comment to the top of `mur-common/src/conversation.rs`:

```rust
//! # Schema versioning
//!
//! Every `Message` carries `v: u32` (current: 1). This is intentional — schema
//! evolution for a durable archive must be explicit.
//!
//! ## When to bump
//! Bump `CONVERSATION_SCHEMA_VERSION` ONLY when:
//!   1. A required field is renamed or removed, OR
//!   2. A field's semantic meaning changes (e.g., `ts` interpretation shifts
//!      from Utc to local), OR
//!   3. `Content` gains a variant that older deserializers cannot safely
//!      ignore.
//!
//! Adding a new optional field with `#[serde(default)]` does NOT require a bump.
//!
//! ## How to bump
//!   1. Add `conversations::migrate_schema::v{N}_to_v{N+1}` that takes any
//!      JSON row and rewrites to the new schema.
//!   2. Wire it into `store::append_with_migration()` so older lines in the
//!      same file still deserialize (via serde `untagged` or custom visitor).
//!   3. Keep the previous version's deserializer functional for at least
//!      one minor release.
//!   4. Bump `CONVERSATION_SCHEMA_VERSION`.
//!
//! ## Backward reads
//! `store::read_day` must always call `migrate_row(json, from_v, current_v)`
//! before building a `Message` so older on-disk rows still work.
```

No code change for Phase 1; the doc is the deliverable. Task 1 commit message can note: "docs: specify schema bump protocol for conversation archive".

---

## BP6 — Observability spans

**Base plan issue:** Pipeline stages are silent on timing/outcome. Hard to debug performance issues in production.

### BP6.1 — Patch Task 9 (pipeline)

Wrap each stage in a tracing span:

```rust
use tracing::info_span;

pub fn process_and_store(
    root: &Path,
    cfg: &ConversationsConfig,
    messages: Vec<Message>,
) -> Result<(usize, usize)> {
    let _guard = info_span!("conversations.pull", count = messages.len()).entered();
    // … acquire pull lock …

    let messages = {
        let _s = info_span!("conversations.normalize").entered();
        let mut m = messages;
        for msg in &mut m {
            if let Err(e) = normalize::substitute(root, msg) {
                tracing::warn!("normalize failed: {e:?}");
            }
        }
        m
    };

    let messages = {
        let _s = info_span!("conversations.dedup", threshold = cfg.filter.dedup_threshold).entered();
        dedup::dedup_batch(messages, cfg.filter.dedup_threshold)
    };

    let (kept, rejected) = {
        let _s = info_span!("conversations.filter").entered();
        let mut kept = Vec::with_capacity(messages.len());
        let mut rejected = 0usize;
        for m in messages {
            if filter::evaluate(&m, &cfg.filter).keep {
                kept.push(m);
            } else {
                rejected += 1;
            }
        }
        (kept, rejected)
    };

    let written = {
        let _s = info_span!("conversations.store", count = kept.len()).entered();
        // … write loop
        kept.len()
    };

    tracing::info!(
        kept = written, rejected = rejected,
        "conversations pull complete"
    );
    Ok((written, rejected))
}
```

Usage: `RUST_LOG=mur_core::conversations=info,mur_core::conversations=debug mur conversations pull` prints per-stage timing.

### BP6.2 — Patch Task 16 (index)

Wrap `upsert` and `search`:

```rust
pub async fn upsert(&self, batch: &[(&Message, Vec<f32>, i8)]) -> Result<()> {
    let _span = tracing::info_span!("index.upsert", count = batch.len()).entered();
    // … existing body
}

pub async fn search(&self, query_vec: Vec<f32>, k: usize, source_filter: Option<&str>) -> Result<Vec<IndexHit>> {
    let _span = tracing::info_span!("index.search", k, source = ?source_filter).entered();
    // … existing body
}
```

---

# Part 3 — Execution Checklist

When executing the base plan, apply these patches **before** running each affected task:

- [ ] **Task 1:** Add `fs2 = "0.4"`, `toml = "0.8"` to deps (P3, P4). Append schema-bump doc comment (BP5).
- [ ] **Task 4:** Use bridge-aware `AuditEntry` + `AuditAction::Migrate` with `bridged_from_hash` (P1.1). Add serialization test (P1.4).
- [ ] **Task 9:** Add `.pull.lock` concurrency guard (BP4). Wrap stages in `tracing::info_span!` (BP6).
- [ ] **Task 16:** Wrap `upsert`/`search` in spans (BP6.2).
- [ ] **Task 17:** Replace `--dry-run` flag with `--run` default-dry (BP2). Add `--resume`, `--discard-staging`, `Preflight` variants (BP3, BP1). Add doctor drift check (P4.3).
- [ ] **Task 19:** Use bridge-aware audit recording via `read_commander_last_hash` (P1.2). Use flock-based `refuse_if_daemon_running` (P3.2). Fail on existing staging dir instead of nuking (BP3.1). Add `sync_commander_config` call after atomic rename (P4.2). Add `resume` function (BP3.3). Add bridge test (P1.4).
- [ ] **Task 21:** Replace `audit_chain_valid` with `bridge_ready` + `bridged_from_hash` in `MigrationPlan` (P1.3).
- [ ] **Task 22:** Assert 6 call sites (P2.1). Add `pattern_handler.rs:48` mapping with `Role::System` (P2.2). Update daemon-lock test to flock-based (P3.3).
- [ ] **Task 23:** Split into 23a (mur yaml) and 23b (commander toml `[conversations]` with `sync_defaults()`) (P4.1).
- [ ] **New Task 19b:** `mur conversations preflight` wiring (BP1).

---

# Self-Review

**Placeholder scan:** No TBD/TODO in this amendments doc. Each patch provides complete code or exact shell commands.

**Internal consistency:**
- `AuditAction::Migrate` fields consistent across P1.1, P1.2, P1.4, P1.3.
- `refuse_if_daemon_running` signature stable across P3.2, P3.3, and BP1's exposed `check_daemon_status`.
- `ConversationsConfig.retention_days` (u32) consistent with commander's `ConversationsSection.retention_days` (u32) in P4.1.
- `--run` default-dry flow (BP2) composes cleanly with `--resume` and `--discard-staging` (BP3).

**Scope check:** Amendments stay strictly within Phase 1 scope. Every patch targets an existing task or adds a single Task 19b; no Phase 2/3 feature added.

**Ambiguity:** `commander_dir` (`~/.mur/commander/`) and `conversations_dir` (`~/.mur/conversations/`) used consistently. `bridged_from_hash` = commander's last entry's `entry_hash` (opaque to mur), never verified by mur's algorithm.

---

**Amendments complete.** Read alongside `2026-04-19-mur-conversations-phase-1.md`.
