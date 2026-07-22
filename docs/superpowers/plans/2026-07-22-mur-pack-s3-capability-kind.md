# MUR Pack S3 — Capability Kind Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a standalone, installable **capability** — MCP server(s) + skill refs + `requires_programs` + suggested entitlements — that an agent can also declare as a dependency, and ship the **media** capability as the first instance.

**Architecture:** Three tasks. (1) `mur-common`: a `Capability` type + `CapabilityEntitlements` + a `requires_capabilities` field on `AgentProfile`. (2) `mur-core`: a compiled-in `media` capability (`include_str!`) + a `builtin_capabilities()` accessor. (3) `mur-core`: `mur capability {list|show|install|remove}` — install materializes the capability into an agent's profile (MCP upsert + `requires_programs` merge + entitlement union behind consent + `requires_capabilities`), remove reverses the MCP wiring. CLI-only; reuses `McpServerEntry`, `ProgramDep`, `Entitlements`.

**Tech Stack:** Rust (edition 2024), `mur-common` (`agent`, `deps`), `mur-core` (`cmd`, `cli`, `dispatch`), `serde_yaml_ng`, clap, `tempfile`.

## Global Constraints

- Reuse existing types — no new copies:
  - `McpServerEntry` (`mur-common/src/agent.rs:256`) derives `Default`; build with `McpServerEntry { name, command, args, timeout_secs, ..Default::default() }`.
  - `ProgramDep` (`mur-common/src/deps/mod.rs:13`) = `{ name, detect: DetectMethod, reason, hint: Option<String>, registry: Option<String>, recipe: Option<ProgramRecipe> }`. `DetectMethod` (untagged): `File { file: String }` | `Command { command: String }` | `Version { version: VersionCheck }`.
  - `Entitlements` (`mur-common/src/agent.rs:546`): `.processes.spawn.allowed: Vec<String>`, `.network.outbound.allow_hosts: Vec<String>`, `.filesystem.read: Vec<String>`.
  - Profile helpers: `crate::cmd::agent::{load_profile_for_edit(name) -> Result<(PathBuf, AgentProfile)>, save_profile(&path, &mut profile), resolve_mur_home() -> Result<PathBuf>}`.
  - MCP command for the media server is the bare `"mur-mcp-server"` (stdio, no args); MCP materialization also appends the command to `entitlements.processes.spawn.allowed` (mirror `cmd_mcp_add`, `mur-core/src/cmd/agent/mcp.rs:148-181`).
- Brand "MUR" uppercase in user-facing strings; internal `name`/ids lowercase.
- Idempotent install (upsert MCP by `name`). `remove` keeps entitlements + `requires_programs` (may be shared), removes only the MCP wiring + the `requires_capabilities` entry.
- Test: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUSTFLAGS=-Cdebuginfo=0 CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target cargo test -p <crate> <name>` (`-p mur-common` Task 1, `-p mur-core` Tasks 2-3; add `~/.rustup/toolchains/stable-aarch64-apple-darwin/bin` to PATH if `cargo` missing). Cold compile takes minutes. NOTE: disk is tight — do not run a full `cargo build --workspace`; build only the crate under test.

---

### Task 1: `Capability` type + `requires_capabilities` field (`mur-common`)

**Files:**
- Create: `mur-common/src/capability.rs`
- Modify: `mur-common/src/lib.rs` (add `pub mod capability;`)
- Modify: `mur-common/src/agent.rs` (add `requires_capabilities` to `AgentProfile`)
- Test: `mur-common/src/capability.rs` tests + `mur-common/src/agent.rs` tests

**Interfaces:**
- Produces: `Capability { name, version, description, mcp_servers: Vec<McpServerEntry>, skills: Vec<String>, requires_programs: Vec<ProgramDep>, entitlements: CapabilityEntitlements }`; `CapabilityEntitlements { spawn_programs, network_hosts, filesystem_read: Vec<String> }`; `AgentProfile.requires_capabilities: Vec<String>`. Consumed by Tasks 2-3.

- [ ] **Step 1: Write `capability.rs` (module + tests)**

Create `mur-common/src/capability.rs`:

```rust
//! A `Capability`: a standalone-installable bundle of MCP server(s) + skill
//! refs + external-program requirements + suggested entitlements. Reuses the
//! existing agent primitives; installed into an agent's profile.

use crate::agent::McpServerEntry;
use crate::deps::ProgramDep;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capability {
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerEntry>,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub requires_programs: Vec<ProgramDep>,
    #[serde(default)]
    pub entitlements: CapabilityEntitlements,
}

/// Suggested entitlements a capability requests at install; each list is
/// unioned into the agent's `Entitlements` after consent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CapabilityEntitlements {
    #[serde(default)]
    pub spawn_programs: Vec<String>,
    #[serde(default)]
    pub network_hosts: Vec<String>,
    #[serde(default)]
    pub filesystem_read: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_yaml_round_trips() {
        let yaml = "\
name: demo
version: 1.0.0
description: a demo capability
skills:
  - foo-skill
requires_programs:
  - name: vlc
    detect:
      command: vlc
    reason: needed for playback
entitlements:
  network_hosts:
    - 127.0.0.1
";
        let c: Capability = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(c.name, "demo");
        assert_eq!(c.skills, vec!["foo-skill"]);
        assert_eq!(c.requires_programs.len(), 1);
        assert_eq!(c.requires_programs[0].name, "vlc");
        assert_eq!(c.entitlements.network_hosts, vec!["127.0.0.1"]);
        let back = serde_yaml_ng::to_string(&c).unwrap();
        let c2: Capability = serde_yaml_ng::from_str(&back).unwrap();
        assert_eq!(c, c2);
    }
}
```

- [ ] **Step 2: Write the `requires_capabilities` test**

Add to `mur-common/src/agent.rs` `#[cfg(test)] mod`:

```rust
#[test]
fn requires_capabilities_defaults_empty_and_round_trips() {
    let base = "name: a\ndisplay_name: A\nmodel_ref: m\n";
    let p: AgentProfile = serde_yaml_ng::from_str(base).unwrap();
    assert!(p.requires_capabilities.is_empty());
    let with = format!("{base}requires_capabilities:\n  - media\n");
    let p2: AgentProfile = serde_yaml_ng::from_str(&with).unwrap();
    assert_eq!(p2.requires_capabilities, vec!["media"]);
}
```
(If the minimal YAML fails to parse for lack of required fields, expand it to a valid profile — keep the field absent in case 1 and `[media]` in case 2.)

- [ ] **Step 3: Run to verify failure**

Run: `… cargo test -p mur-common capability_yaml_round_trips`
Expected: FAIL — `mur_common::capability` module and `requires_capabilities` field don't exist.

- [ ] **Step 4: Wire the module + field**

- Add to `mur-common/src/lib.rs`: `pub mod capability;`
- Add to `AgentProfile` in `mur-common/src/agent.rs` (next to `requires_programs` ~line 157):

```rust
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_capabilities: Vec<String>,
```

If any `AgentProfile { .. }` literal without `..Default::default()` breaks, add `requires_capabilities: Vec::new()` there (mirror how `requires_programs` is handled at those sites).

- [ ] **Step 5: Run to verify pass**

Run: `… cargo test -p mur-common capability` and `… cargo test -p mur-common requires_capabilities`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/capability.rs mur-common/src/lib.rs mur-common/src/agent.rs
git commit -m "feat(common): Capability type + requires_capabilities agent field"
```

---

### Task 2: Builtin `media` capability (`mur-core`)

**Files:**
- Create: `mur-core/src/capabilities/media.yaml`
- Create: `mur-core/src/capabilities/mod.rs`
- Modify: `mur-core/src/lib.rs` (add `pub mod capabilities;`)
- Test: `mur-core/src/capabilities/mod.rs` tests

**Interfaces:**
- Consumes: `mur_common::capability::Capability` (Task 1).
- Produces: `pub fn builtin_capabilities() -> Vec<Capability>`, `pub fn find_builtin(name: &str) -> Option<Capability>`. Consumed by Task 3.

- [ ] **Step 1: Write `capabilities/mod.rs` (accessor + test)**

Create `mur-core/src/capabilities/mod.rs`:

```rust
//! Compiled-in capabilities shipped with the binary (like builtin skills).

use mur_common::capability::Capability;

const MEDIA_YAML: &str = include_str!("media.yaml");

/// All capabilities shipped with this binary.
pub fn builtin_capabilities() -> Vec<Capability> {
    vec![serde_yaml_ng::from_str(MEDIA_YAML).expect("builtin media capability must parse")]
}

/// Look up a builtin capability by name.
pub fn find_builtin(name: &str) -> Option<Capability> {
    builtin_capabilities().into_iter().find(|c| c.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_capability_parses_and_bundles_the_media_pieces() {
        let media = find_builtin("media").expect("media capability present");
        assert_eq!(media.name, "media");
        for s in ["video-analyze", "watch-together", "scene-explain", "vlc-control"] {
            assert!(media.skills.iter().any(|x| x == s), "missing skill {s}");
        }
        for p in ["vlc", "yt-dlp"] {
            assert!(media.requires_programs.iter().any(|d| d.name == p), "missing dep {p}");
        }
        assert_eq!(media.mcp_servers.len(), 1);
        assert_eq!(media.mcp_servers[0].command, "mur-mcp-server");
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `… cargo test -p mur-core media_capability_parses`
Expected: FAIL — module / `media.yaml` don't exist.

- [ ] **Step 3: Create `media.yaml` + register the module**

Create `mur-core/src/capabilities/media.yaml`:

```yaml
name: media
version: 1.0.0
description: 'Watch and analyze video with MUR: control VLC, co-watch, and summarize videos via the local multimodal model.'
mcp_servers:
- name: media
  command: mur-mcp-server
  timeout_secs: 300
skills:
- video-analyze
- watch-together
- scene-explain
- vlc-control
requires_programs:
- name: vlc
  detect:
    file: /Applications/VLC.app/Contents/MacOS/VLC
  reason: VLC is required to open and control video playback.
  hint: Install VLC from https://www.videolan.org/vlc/ (or `brew install --cask vlc`).
- name: yt-dlp
  detect:
    command: yt-dlp
  reason: yt-dlp fetches captions/streams for YouTube analysis.
  hint: '`brew install yt-dlp` or see https://github.com/yt-dlp/yt-dlp'
entitlements:
  network_hosts:
  - 127.0.0.1
```

Add to `mur-core/src/lib.rs`: `pub mod capabilities;`

- [ ] **Step 4: Run to verify pass**

Run: `… cargo test -p mur-core media_capability_parses`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/capabilities/media.yaml mur-core/src/capabilities/mod.rs mur-core/src/lib.rs
git commit -m "feat(capability): compiled-in media capability + builtin accessor"
```

---

### Task 3: `mur capability {list|show|install|remove}` (`mur-core`)

**Files:**
- Create: `mur-core/src/cmd/capability.rs`
- Modify: `mur-core/src/cmd/mod.rs` (add `pub mod capability;`)
- Modify: `mur-core/src/cli/mod.rs` (add `Capability` to `Commands`)
- Modify: `mur-core/src/cli/actions.rs` (add `CapabilityAction`)
- Modify: `mur-core/src/dispatch.rs` (add the dispatch arm + import)
- Test: `mur-core/src/cmd/capability.rs` tests

**Interfaces:**
- Consumes: `crate::capabilities::{builtin_capabilities, find_builtin}` (Task 2); `Capability`; `McpServerEntry`; profile helpers.
- Produces: `install_capability(home, agent, cap) -> Result<bool>`, `remove_capability(home, agent, cap) -> Result<bool>`, and `cmd_capability_{list,show,install,remove}`.

- [ ] **Step 1: Write `cmd/capability.rs` (core + tests)**

Create `mur-core/src/cmd/capability.rs`:

```rust
//! `mur capability` — install a bundled capability (MCP + skills + programs +
//! entitlements) into an agent, or list/show/remove it.

use anyhow::{Result, bail};
use mur_common::AgentProfile as _AgentProfile;
use mur_common::capability::Capability;
use std::path::Path;

fn load(home: &Path, agent: &str) -> Result<(std::path::PathBuf, _AgentProfile)> {
    let path = home.join("agents").join(agent).join("profile.yaml");
    if !path.exists() {
        bail!("agent '{agent}' not found");
    }
    let profile = serde_yaml_ng::from_str(&std::fs::read_to_string(&path)?)?;
    Ok((path, profile))
}

fn union_extend(dst: &mut Vec<String>, src: &[String]) {
    for s in src {
        if !dst.iter().any(|d| d == s) {
            dst.push(s.clone());
        }
    }
}

/// Materialize `cap` into agent `agent` under `home`. Idempotent.
pub(crate) fn install_capability(home: &Path, agent: &str, cap: &Capability) -> Result<bool> {
    let (path, mut profile) = load(home, agent)?;
    // 1. MCP servers: upsert by name + allow the command to be spawned.
    for entry in &cap.mcp_servers {
        profile.mcp_servers.retain(|s| s.name != entry.name);
        profile.mcp_servers.push(entry.clone());
        if !profile.entitlements.processes.spawn.allowed.iter().any(|a| a == &entry.command) {
            profile.entitlements.processes.spawn.allowed.push(entry.command.clone());
        }
    }
    // 2. requires_programs: merge (dedup by name).
    for dep in &cap.requires_programs {
        if !profile.requires_programs.iter().any(|d| d.name == dep.name) {
            profile.requires_programs.push(dep.clone());
        }
    }
    // 3. entitlements union.
    union_extend(&mut profile.entitlements.processes.spawn.allowed, &cap.entitlements.spawn_programs);
    union_extend(&mut profile.entitlements.network.outbound.allow_hosts, &cap.entitlements.network_hosts);
    union_extend(&mut profile.entitlements.filesystem.read, &cap.entitlements.filesystem_read);
    // 4. requires_capabilities.
    if !profile.requires_capabilities.iter().any(|c| c == &cap.name) {
        profile.requires_capabilities.push(cap.name.clone());
    }
    crate::cmd::agent::save_profile(&path, &mut profile)?;
    Ok(true)
}

/// Remove the MCP wiring `cap` added + drop it from `requires_capabilities`.
/// Keeps entitlements + `requires_programs` (may be shared).
pub(crate) fn remove_capability(home: &Path, agent: &str, cap: &Capability) -> Result<bool> {
    let (path, mut profile) = load(home, agent)?;
    let names: Vec<&str> = cap.mcp_servers.iter().map(|s| s.name.as_str()).collect();
    profile.mcp_servers.retain(|s| !names.contains(&s.name.as_str()));
    profile.requires_capabilities.retain(|c| c != &cap.name);
    crate::cmd::agent::save_profile(&path, &mut profile)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_agent(home: &Path, agent: &str) {
        let dir = home.join("agents").join(agent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("profile.yaml"), "name: a\ndisplay_name: A\nmodel_ref: m\n").unwrap();
    }
    fn media() -> Capability {
        crate::capabilities::find_builtin("media").unwrap()
    }

    #[test]
    fn install_materializes_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        seed_agent(home, "a");
        install_capability(home, "a", &media()).unwrap();
        install_capability(home, "a", &media()).unwrap();
        let p: _AgentProfile = serde_yaml_ng::from_str(
            &std::fs::read_to_string(home.join("agents/a/profile.yaml")).unwrap()).unwrap();
        assert_eq!(p.mcp_servers.iter().filter(|s| s.name == "media").count(), 1);
        assert!(p.requires_programs.iter().any(|d| d.name == "vlc"));
        assert!(p.requires_capabilities.iter().any(|c| c == "media"));
        assert!(p.entitlements.network.outbound.allow_hosts.iter().any(|h| h == "127.0.0.1"));
        assert!(p.entitlements.processes.spawn.allowed.iter().any(|c| c == "mur-mcp-server"));
    }

    #[test]
    fn remove_reverses_mcp_and_requires_capabilities_but_keeps_programs() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        seed_agent(home, "a");
        install_capability(home, "a", &media()).unwrap();
        remove_capability(home, "a", &media()).unwrap();
        let p: _AgentProfile = serde_yaml_ng::from_str(
            &std::fs::read_to_string(home.join("agents/a/profile.yaml")).unwrap()).unwrap();
        assert!(!p.mcp_servers.iter().any(|s| s.name == "media"));
        assert!(!p.requires_capabilities.iter().any(|c| c == "media"));
        assert!(p.requires_programs.iter().any(|d| d.name == "vlc"));
    }
}
```

(If the seed `profile.yaml` lacks required fields, expand to a valid minimal profile.)

- [ ] **Step 2: Run to verify failure**

Run: `… cargo test -p mur-core install_materializes_and_is_idempotent`
Expected: FAIL — `crate::cmd::capability` / `crate::capabilities` not declared.

- [ ] **Step 3: Add the command fns + register the cmd module**

Append to `mur-core/src/cmd/capability.rs`:

```rust
/// `mur capability list [--agent X]`
pub fn cmd_capability_list(agent: Option<&str>) -> Result<()> {
    let installed: Vec<String> = match agent {
        Some(a) => crate::cmd::agent::load_profile_for_edit(a)?.1.requires_capabilities,
        None => Vec::new(),
    };
    for c in crate::capabilities::builtin_capabilities() {
        let mark = if installed.iter().any(|n| n == &c.name) { " [installed]" } else { "" };
        println!("{}  {}{}", c.name, c.description, mark);
    }
    Ok(())
}

/// `mur capability show <name>`
pub fn cmd_capability_show(name: &str) -> Result<()> {
    let cap = crate::capabilities::find_builtin(name)
        .ok_or_else(|| anyhow::anyhow!("capability '{name}' not found"))?;
    print!("{}", serde_yaml_ng::to_string(&cap)?);
    Ok(())
}

/// `mur capability install <name> --agent X [--yes]`
pub fn cmd_capability_install(name: &str, agent: &str, yes: bool) -> Result<()> {
    let cap = crate::capabilities::find_builtin(name)
        .ok_or_else(|| anyhow::anyhow!("capability '{name}' not found"))?;
    if !confirm_install(&cap, yes)? {
        bail!("install cancelled");
    }
    let home = crate::cmd::agent::resolve_mur_home()?;
    install_capability(&home, agent, &cap)?;
    println!("Installed capability '{name}' onto '{agent}'. Restart the agent to apply.");
    Ok(())
}

/// `mur capability remove <name> --agent X`
pub fn cmd_capability_remove(name: &str, agent: &str) -> Result<()> {
    let cap = crate::capabilities::find_builtin(name)
        .ok_or_else(|| anyhow::anyhow!("capability '{name}' not found"))?;
    let home = crate::cmd::agent::resolve_mur_home()?;
    remove_capability(&home, agent, &cap)?;
    println!("Removed capability '{name}' from '{agent}' (MCP + requires_capabilities). Entitlements and program requirements kept. Restart the agent to apply.");
    Ok(())
}

fn confirm_install(cap: &Capability, yes: bool) -> Result<bool> {
    if yes {
        return Ok(true);
    }
    println!("Capability '{}' will grant on this agent:", cap.name);
    for e in &cap.mcp_servers {
        println!("  MCP server: {} ({})", e.name, e.command);
    }
    for d in &cap.requires_programs {
        println!("  requires program: {}", d.name);
    }
    if !cap.entitlements.network_hosts.is_empty() {
        println!("  network hosts: {}", cap.entitlements.network_hosts.join(", "));
    }
    use std::io::{self, IsTerminal, Write};
    if !io::stdin().is_terminal() {
        bail!("not a TTY — re-run with --yes to install non-interactively");
    }
    print!("Proceed? [y/N] ");
    io::stdout().flush().ok();
    let mut ans = String::new();
    io::stdin().read_line(&mut ans)?;
    Ok(matches!(ans.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}
```

Add to `mur-core/src/cmd/mod.rs`: `pub mod capability;`

- [ ] **Step 4: Register the CLI command + dispatch**

In `mur-core/src/cli/mod.rs` `Commands` enum (mirror `Fleet`):

```rust
    /// Manage capabilities — installable bundles of MCP + skills + programs
    Capability {
        #[command(subcommand)]
        action: CapabilityAction,
    },
```

In `mur-core/src/cli/actions.rs` (mirror `FleetAction`):

```rust
#[derive(clap::Subcommand, Debug)]
pub enum CapabilityAction {
    /// List available capabilities
    List {
        #[arg(long)]
        agent: Option<String>,
    },
    /// Show a capability's contents
    Show { name: String },
    /// Install a capability onto an agent
    Install {
        name: String,
        #[arg(long)]
        agent: String,
        #[arg(long)]
        yes: bool,
    },
    /// Remove a capability from an agent
    Remove {
        name: String,
        #[arg(long)]
        agent: String,
    },
}
```

In `mur-core/src/dispatch.rs`: import `CapabilityAction` next to the `FleetAction` import (~line 14), and add the arm (mirror the `Fleet` arm ~line 269):

```rust
        Commands::Capability { action } => match action {
            CapabilityAction::List { agent } => {
                cmd::capability::cmd_capability_list(agent.as_deref())?
            }
            CapabilityAction::Show { name } => cmd::capability::cmd_capability_show(&name)?,
            CapabilityAction::Install { name, agent, yes } => {
                cmd::capability::cmd_capability_install(&name, &agent, yes)?
            }
            CapabilityAction::Remove { name, agent } => {
                cmd::capability::cmd_capability_remove(&name, &agent)?
            }
        },
```

- [ ] **Step 5: Run tests + clippy**

Run:
```
… cargo test -p mur-core install_materializes_and_is_idempotent
… cargo test -p mur-core remove_reverses
… cargo clippy -p mur-core -- -D warnings
```
Expected: tests PASS; clippy clean (the CLI enum/dispatch compile). If a debug bin CLI-parse test stack-overflows, set `RUST_MIN_STACK=33554432` (known pre-existing gotcha, not this diff).

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/capability.rs mur-core/src/cmd/mod.rs mur-core/src/cli/mod.rs mur-core/src/cli/actions.rs mur-core/src/dispatch.rs
git commit -m "feat(capability): mur capability list/show/install/remove"
```

---

## Rollout / usage (post-merge)

`mur capability list` shows `media`. `mur capability install media --agent <name>` wires the media MCP server + VLC/yt-dlp requirements + loopback host into the agent (behind a consent prompt), records `requires_capabilities: [media]`; restart the agent to apply. `mur agent doctor` surfaces a missing VLC/yt-dlp. `mur capability remove media --agent <name>` reverses the MCP wiring.

## Self-Review

**Spec coverage:** §4.1 type → Task 1; §4.2 media builtin → Task 2; §4.3 `requires_capabilities` → Task 1; §4.4 CLI + materialization → Task 3; §4.5 consent → Task 3 (`confirm_install` + `--yes` + non-TTY bail). ✅

**Placeholder scan:** none — all code/tests complete; `media.yaml` values concrete (VLC `File` detect on the macOS path, yt-dlp `Command` detect, loopback host). Cross-platform VLC detect is noted future in the spec, not a placeholder.

**Type consistency:** `Capability`/`CapabilityEntitlements` (Task 1) consumed verbatim in Tasks 2-3. `install_capability`/`remove_capability` defined in Task 3, used by its tests and command fns. Entitlement field paths (`entitlements.processes.spawn.allowed`, `.network.outbound.allow_hosts`, `.filesystem.read`) match Global Constraints. CLI enum variants match the dispatch arm.

**Scope:** three tasks, CLI-only. Global store, third-party import, runtime-resolve, unified kernel remain out of scope. `ponytail:` install materializes MCP config into the profile — MCP has no global-vs-local shadow structure, so this is config not the S1 vendoring bug; reference-resolution deferred until drift is observed.
