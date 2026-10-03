//! B0 rule 6 / M9.2 — install-time MCP hash + publisher prompt.
//!
//! At `mur agent mcp add` time we compute three artefacts that get
//! pinned into `McpServerEntry`:
//!
//! 1. **Binary SHA-256** — captures the exact bytes of the resolved
//!    `command` path. Detects "the binary on disk changed" attacks
//!    even when the publisher's signature is still valid (rule 11
//!    catches unsigned tampering; rule 6 catches signed-but-evolved).
//! 2. **Description hash** — SHA-256 over canonical-JSON of the MCP's
//!    `tools/list` response. Catches "same binary, different tool
//!    descriptions" — the prompt-injection rug-pull where a malicious
//!    update adds new tools whose descriptions hijack the LLM.
//! 3. **Publisher metadata** — best-effort display string from the
//!    MCP's `initialize` response. Stored for the user's record only;
//!    not validated against any external authority.
//!
//! Helpers in this module are deliberately small + pure-input so the
//! rule-3 startup verifier (M9.3) can reuse them.

use anyhow::{Context, Result, bail};
use mur_common::agent::{McpPublisherInfo, McpServerEntry};
use mur_common::canonical;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Resolve `command` to an absolute path on disk.
///
/// - If `command` is already absolute, canonicalise it (resolves
///   symlinks).
/// - Otherwise consult `PATH`. Returns the first match found.
///
/// Returns an error if the binary can't be located. Used by both
/// install-time hashing and startup verification so a bare `command`
/// like "mcp-weather" stays consistent across the two passes — including
/// the PATH they are resolved against, which is the augmented one on both
/// sides (see [`mur_common::exec::resolve_command`]).
pub fn resolve_command(command: &str) -> Result<PathBuf> {
    // Single source of truth in mur-common so install-time pinning and the
    // runtime startup verification (B0 rules 6 & 11) resolve identically.
    mur_common::exec::resolve_command(command)
}

/// Stream-hash the file at `path` with SHA-256. Returns lowercase
/// hex. Reads in 64 KiB chunks so large MCP binaries don't allocate
/// the whole file into memory.
pub fn compute_binary_sha256(path: &Path) -> Result<String> {
    use std::fs::File;
    use std::io::Read;

    let mut f = File::open(path).with_context(|| format!("open binary at {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f
            .read(&mut buf)
            .with_context(|| format!("read binary at {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Compute the description hash from a `tools/list` response.
///
/// The hash covers `{ "tools": [<each tool's name + description +
/// input_schema>] }` in canonical-JSON form, which is what the
/// startup verifier in M9.3 will recompute. Tool order from the
/// upstream MCP is preserved (the spec reserves it as significant).
///
/// Wired into `cmd_mcp_pin` in M9.3.5 via `probe_mcp_descriptions`.
pub fn compute_description_hash(tools: &[McpToolDescription]) -> String {
    let value = serde_json::json!({
        "tools": tools.iter().map(|t| serde_json::json!({
            "name": t.name,
            "description": t.description,
            "input_schema": t.input_schema,
        })).collect::<Vec<_>>(),
    });
    let bytes = canonical::canonical_json(&value);
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hex::encode(hasher.finalize())
}

/// One tool entry as shown to the user during install confirmation
/// and as fed into the description hash. Mirrors `mur-agent-runtime::
/// protocol::mcp_client::ToolInfo` but lives in mur-core so the
/// install path doesn't need to spawn a runtime.
#[derive(Debug, Clone, serde::Serialize)]
pub struct McpToolDescription {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

impl From<mur_agent_runtime::protocol::mcp_client::ToolInfo> for McpToolDescription {
    fn from(t: mur_agent_runtime::protocol::mcp_client::ToolInfo) -> Self {
        Self {
            name: t.name,
            description: t.description,
            input_schema: t.input_schema,
        }
    }
}

/// Default budget for the live MCP probe (handshake, initialize,
/// tools/list, shutdown). Override via `MUR_MCP_PROBE_TIMEOUT_S` for
/// long-startup MCPs (model warm-up, network discovery).
pub const DEFAULT_PROBE_TIMEOUT_SECS: u64 = 10;

/// Read the probe timeout from `MUR_MCP_PROBE_TIMEOUT_S` env var,
/// falling back to `DEFAULT_PROBE_TIMEOUT_SECS`. Invalid values
/// (non-integer, zero) are silently ignored — better to fall back
/// than refuse a probe over a bad env var.
pub fn probe_timeout() -> std::time::Duration {
    let secs = std::env::var("MUR_MCP_PROBE_TIMEOUT_S")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_PROBE_TIMEOUT_SECS);
    std::time::Duration::from_secs(secs)
}

/// Errors a live MCP probe can return. The CLI maps each onto a
/// distinct user-facing message.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("probe timed out after {0:?} (raise MUR_MCP_PROBE_TIMEOUT_S to extend)")]
    Timeout(std::time::Duration),
    #[error("MCP spawn / handshake failed: {0}")]
    Mcp(#[from] mur_agent_runtime::protocol::mcp_client::McpError),
    /// Probe setup failed (runtime, egress proxy, thread); the server never ran.
    #[error("{0}")]
    Setup(String),
}

/// Spawn the MCP, run `initialize` + `tools/list` + `shutdown`, and
/// return the canonical description hash plus the raw tools list.
///
/// Callers go through `probe_egress::probe_as_runtime_would`, which supplies
/// the egress proxy a `Restricted` entry needs and owns its lifetime (#1647).
pub async fn probe_mcp_descriptions(
    entry: &McpServerEntry,
    timeout: std::time::Duration,
    policy: &mur_agent_runtime::sandbox::policy::SandboxPolicy,
    proxy: Option<&mur_agent_runtime::sandbox::egress_proxy::EgressProxyHandle>,
) -> Result<
    (
        String,
        Vec<mur_agent_runtime::protocol::mcp_client::ToolInfo>,
    ),
    ProbeError,
> {
    let probe = async {
        // The caller chooses the policy, and the choice is the whole point of
        // the probe. `inspect` passes the agent's real one so a server that
        // dies on a sandbox EPERM fails here too (#1161); the pin path passes a
        // permissive one, where the job is only to hash tool descriptions.
        let mut client =
            mur_agent_runtime::protocol::mcp_client::McpClient::connect(entry, policy, proxy)
                .await?;
        let _info = client.initialize().await?;
        let tools = client.list_tools().await?;
        client.shutdown().await;
        Ok::<_, mur_agent_runtime::protocol::mcp_client::McpError>(tools)
    };

    let tools = match tokio::time::timeout(timeout, probe).await {
        Ok(Ok(tools)) => tools,
        Ok(Err(e)) => return Err(ProbeError::Mcp(e)),
        Err(_) => return Err(ProbeError::Timeout(timeout)),
    };

    let descriptions: Vec<McpToolDescription> = tools.iter().cloned().map(Into::into).collect();
    let hash = compute_description_hash(&descriptions);
    Ok((hash, tools))
}

/// Build a fully-populated `McpServerEntry` for a fresh install.
///
/// Caller is responsible for actually probing the MCP via stdio +
/// rendering the user-facing confirmation prompt. This helper is the
/// pure assembly step so it can be unit-tested without an MCP fixture.
/// Currently used only by tests; M9.3.5 will switch `cmd_mcp_pin` to
/// use it once the live description-probe lands.
#[allow(dead_code)] // wired by M9.3.5 (description-hash live probe)
pub fn build_pinned_entry(
    name: &str,
    command: &str,
    args: &[String],
    binary_sha256: String,
    description_hash: String,
    publisher: Option<McpPublisherInfo>,
) -> McpServerEntry {
    McpServerEntry {
        name: name.to_string(),
        command: command.to_string(),
        args: args.to_vec(),
        binary_sha256: Some(binary_sha256),
        description_hash: Some(description_hash),
        publisher,
        installed_at: Some(chrono::Utc::now()),
        timeout_secs: None,
        network: None,
        url: None,
        auth: None,
        requires_programs: Vec::new(),
        state_paths: Vec::new(),
        package: None,
    }
}

// ───────────────────────────────────────────────────────────────────
// `mur agent mcp inspect` + `mur agent mcp pin` (B0 rule 6 / M9.4)
// ───────────────────────────────────────────────────────────────────

/// `mur agent mcp inspect <name> [--server <id>]`. Without `--server`,
/// prints all MCPs on the agent and returns the WORST status (highest
/// numeric value) so a scripted caller knows whether ANY MCP drifted.
pub fn cmd_mcp_inspect(
    name: &str,
    server_id: Option<&str>,
    probe: bool,
    deep: bool,
) -> Result<i32> {
    let (_path, profile) = crate::cmd::agent::load_profile_for_edit(name)?;
    if profile.mcp_servers.is_empty() {
        println!("Agent `{name}` has no MCP servers configured.");
        return Ok(0);
    }
    // The probe is only worth running if it runs under the same policy the
    // supervisor will apply; a permissive probe reports CLEAN for exactly the
    // servers that die on startup (#1161).
    let agent_home = crate::cmd::agent::resolve_mur_home()?
        .join("agents")
        .join(name);
    let probe_policy = mur_agent_runtime::sandbox::policy::SandboxPolicy::from_entitlements(
        &profile.entitlements,
        &agent_home,
    );
    let mut worst: u8 = 0;
    let mut printed = false;
    for entry in &profile.mcp_servers {
        if let Some(id) = server_id
            && entry.name != id
        {
            continue;
        }
        if printed {
            println!();
        }
        let status = if probe {
            inspect_one_probed(name, entry, probe_timeout(), &probe_policy) as u8
        } else {
            inspect_one(name, entry) as u8
        };
        worst = worst.max(status);

        // The deep pass re-derives the tree from the registry, so it can see
        // what a locally-stored hash cannot: an edited file leaves the lockfile
        // — and therefore the startup pin — untouched.
        if deep {
            match &entry.package {
                Some(pkg) => {
                    println!();
                    let clean = crate::cmd::agent_mcp_deep_audit::report(&entry.name, pkg);
                    if !clean {
                        worst = worst.max(InspectStatus::BinaryDrift as u8);
                    }
                }
                None => {
                    println!("  deep audit:     n/a — only vendored entries can be re-derived");
                }
            }
        }
        printed = true;
    }
    if !printed && let Some(id) = server_id {
        bail!("MCP server `{id}` not found on agent `{name}`");
    }
    Ok(worst as i32)
}

/// `mur agent mcp pin <name> --server <id> [--force] [--no-probe]`.
/// Re-computes the install-time binary hash and, by default, also
/// spawns the MCP to refresh the description hash (M9.3.5). On probe
/// failure (timeout / spawn error) the pin still lands but
/// `description_hash` stays None — the user sees a warning and can
/// re-pin later.
/// Record the version an interpreter-launched entry currently resolves to,
/// rewriting its args in place. Returns the pinned `name@version` when it
/// changed anything.
///
/// Best-effort by design: no network, no npm, or an unparseable arg shape all
/// leave the entry exactly as it was. A pin that can't reach the registry is a
/// worse reason to fail than to proceed — the binary pin below still applies.
fn pin_package_version(entry: &mut McpServerEntry) -> Option<String> {
    use mur_common::mcp_package::{parse_spec, resolve_current_version, runner_for};

    let spec = parse_spec(&entry.command, &entry.args)?;
    if !spec.floats() {
        return None; // already pinned to a release
    }
    let runner = runner_for(&entry.command)?;
    match resolve_current_version(runner, &spec.name) {
        Ok(version) => {
            let pinned = format!("{}@{version}", spec.name);
            entry.args[spec.arg_index] = pinned.clone();
            Some(pinned)
        }
        Err(e) => {
            eprintln!(
                "warning: could not resolve a current version for `{}` ({e}); \
                 the entry stays on a floating spec and will resolve fresh on every start.",
                spec.name,
            );
            None
        }
    }
}

/// True when re-pinning would write back exactly what the profile already
/// holds. `mur deep-research` re-pins on every run, sometimes from inside an
/// agent sandbox that cannot (and must not) write a sibling agent's profile —
/// so an unchanged pin has to be a no-op on disk, not a rewrite that only
/// bumps `installed_at` and trips the sandbox.
fn pin_unchanged(
    entry: &McpServerEntry,
    new_hash: &str,
    new_description_hash: Option<&str>,
    version_pinned: bool,
    publisher: &Option<mur_common::agent::McpPublisherInfo>,
) -> bool {
    !version_pinned
        && entry
            .binary_sha256
            .as_deref()
            .is_some_and(|old| old.eq_ignore_ascii_case(new_hash))
        && new_description_hash.is_none_or(|h| entry.description_hash.as_deref() == Some(h))
        && entry.publisher == *publisher
}

pub fn cmd_mcp_pin(
    name: &str,
    server_id: &str,
    force: bool,
    no_probe: bool,
    publisher_name: Option<String>,
    publisher_homepage: Option<String>,
    publisher_registry_id: Option<String>,
) -> Result<()> {
    let (path, mut profile) = crate::cmd::agent::load_profile_for_edit(name)?;
    let entry = profile
        .mcp_servers
        .iter_mut()
        .find(|s| s.name == server_id)
        .ok_or_else(|| anyhow::anyhow!("MCP server `{server_id}` not found on agent `{name}`"))?;

    // For an interpreter-launched entry the binary hash below covers the
    // launcher, not the server (#795). The one thing that can be pinned here is
    // *which release* the runner resolves — so record it, turning "whatever the
    // registry serves at spawn" into the version the user approved just now.
    let version_pinned = pin_package_version(entry);

    let resolved = resolve_command(&entry.command)
        .with_context(|| format!("resolve command `{}`", entry.command))?;
    let new_hash = compute_binary_sha256(&resolved)
        .with_context(|| format!("hash binary at {}", resolved.display()))?;

    // Live probe to refresh description_hash. Default on; --no-probe
    // skips for MCPs that can't be cleanly probed (slow boot,
    // side-effecting init, network discovery during startup, etc.).
    // Probe failure persists `None` rather than failing the pin —
    // user can re-pin once the issue is resolved.
    let new_description_hash: Option<String> = if no_probe {
        None
    } else {
        // Build a probe-ready entry with the latest binary so the
        // McpClient spawns the same bytes we just hashed.
        let probe_entry = mur_common::agent::McpServerEntry {
            command: resolved.display().to_string(),
            ..entry.clone()
        };
        // Permissive policy on purpose: this probe exists to hash tool
        // descriptions, and a sandbox denial would drop the hash for a server
        // that is otherwise fine to pin. The proxy is not optional: a
        // fail-closed `Restricted` server never answers `tools/list` without it.
        match probe_egress::probe_as_runtime_would(
            name,
            &probe_entry,
            probe_timeout(),
            &mur_agent_runtime::sandbox::policy::SandboxPolicy::default(),
        ) {
            Ok((hash, tools)) => {
                tracing::info!(
                    mcp = %entry.name,
                    tools = tools.len(),
                    "M9.3.5: live probe captured description hash",
                );
                Some(hash)
            }
            Err(e) => {
                eprintln!(
                    "warning: live MCP probe failed for `{server_id}` ({e}); \
                     pin will record binary hash only — re-run \
                     `mur agent mcp pin {name} {server_id}` once the MCP \
                     is reachable to capture the description hash.",
                );
                None
            }
        }
    };

    // Preserve existing publisher unless any new field is provided
    // (in which case the user's intent is to overwrite the metadata
    // alongside the rehash).
    let publisher = match (publisher_name, publisher_homepage, publisher_registry_id) {
        (None, None, None) => entry.publisher.clone(),
        (n, h, r) => Some(mur_common::agent::McpPublisherInfo {
            name: n.unwrap_or_else(|| {
                entry
                    .publisher
                    .as_ref()
                    .map(|p| p.name.clone())
                    .unwrap_or_default()
            }),
            homepage: h.or_else(|| entry.publisher.as_ref().and_then(|p| p.homepage.clone())),
            registry_id: r.or_else(|| entry.publisher.as_ref().and_then(|p| p.registry_id.clone())),
        }),
    };

    if pin_unchanged(
        entry,
        &new_hash,
        new_description_hash.as_deref(),
        version_pinned.is_some(),
        &publisher,
    ) {
        println!(
            "MCP `{server_id}` on agent `{name}` already pinned to {new_hash} — nothing to write."
        );
        return Ok(());
    }

    if !force {
        println!("Re-approving MCP `{server_id}` on agent `{name}`:");
        println!("  command:        {}", resolved.display());
        if let Some(pinned) = &version_pinned {
            println!("  package:        {pinned}  (NEW — was floating to latest)");
        }
        if let Some(old) = &entry.binary_sha256 {
            if old.eq_ignore_ascii_case(&new_hash) {
                println!("  binary sha256:  {new_hash}  (unchanged)");
            } else {
                println!("  pinned sha256:  {old}  (old)");
                println!("  current sha256: {new_hash}  (NEW — drifted)");
            }
        } else {
            println!("  binary sha256:  {new_hash}  (was unpinned)");
        }
        if let Some(p) = &publisher {
            println!("  publisher:      {}", p.name);
            if let Some(h) = &p.homepage {
                println!("                  {h}");
            }
            if let Some(r) = &p.registry_id {
                println!("                  {r}");
            }
        }
        print!("\nApprove? [y/N] ");
        use std::io::{self, Write};
        io::stdout().flush().ok();
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .with_context(|| "read confirmation from stdin")?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            bail!("re-approval cancelled");
        }
    }

    entry.binary_sha256 = Some(new_hash);
    if let Some(h) = new_description_hash {
        entry.description_hash = Some(h);
    }
    // If probe was skipped or failed, leave existing description_hash
    // intact (so `--no-probe` re-pins don't accidentally erase a
    // previously-captured hash).
    entry.publisher = publisher;
    entry.installed_at = Some(chrono::Utc::now());
    crate::cmd::agent::save_profile(&path, &mut profile)?;
    println!("Re-approved MCP `{server_id}` on agent `{name}`.");
    Ok(())
}

mod inspect;
pub use inspect::{InspectStatus, binary_status, inspect_one, inspect_one_probed};
mod probe_egress;
pub(crate) use probe_egress::probe_as_runtime_would;
#[cfg(test)]
mod tests;
