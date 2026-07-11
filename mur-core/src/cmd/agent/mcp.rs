//! `mur agent mcp` — list / add / remove / rename MCP servers attached to
//! an agent profile.

use anyhow::{Context, Result, bail};
use mur_common::agent::{EgressAuthorization, McpNetMode, McpServerEntry, McpServerNetwork};
use mur_common::telemetry::METHOD_EGRESS_BROAD_AUDITED_ENABLED;

use super::{load_profile_for_edit, resolve_mur_home, save_profile};

/// Optional install-time pinning fields for `cmd_mcp_add` (B0 rule 6 / M9.2).
///
/// `force = true` skips the y/N confirm prompt — for scripted /
/// non-interactive installers. Publisher fields are passed through
/// from `--publisher-name` / `--publisher-homepage` /
/// `--publisher-registry-id` CLI flags; they're display-only and
/// don't affect the binary hash.
#[derive(Debug, Clone, Default)]
pub struct McpAddPin {
    pub force: bool,
    pub publisher_name: Option<String>,
    pub publisher_homepage: Option<String>,
    pub publisher_registry_id: Option<String>,
}

/// Render a single MCP server entry as a formatted line, with warning for BroadAudited mode.
fn render_server_line(s: &McpServerEntry) -> String {
    let base_line = format!("{}\t{} {}", s.name, s.command, s.args.join(" "))
        .trim_end()
        .to_string();

    if let Some(net) = &s.network
        && net.mode == McpNetMode::BroadAudited
    {
        let authorized_by = net
            .authorization
            .as_ref()
            .map(|a| a.authorized_by.as_str())
            .unwrap_or("unknown");
        let warning = format!(
            "\n  ⚠ BROAD EGRESS (audited) — allows any host except deny_hosts; authorized by {}",
            authorized_by
        );
        return base_line + &warning;
    }

    base_line
}

pub fn cmd_mcp_list(name: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    if profile.mcp_servers.is_empty() {
        println!("(no MCP servers configured)");
        return Ok(());
    }
    for s in &profile.mcp_servers {
        println!("{}", render_server_line(s));
    }
    Ok(())
}

pub fn cmd_mcp_add(
    name: &str,
    server_id: &str,
    command: &str,
    args: &[String],
    pin: McpAddPin,
) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if profile.mcp_servers.iter().any(|s| s.name == server_id) {
        bail!("MCP server '{server_id}' already exists on '{name}'");
    }

    // ── B0 rule 6 / M9.2: install-time hash + publisher prompt. ──
    // Best-effort: if the binary can't be located on PATH yet, we
    // proceed without a hash and the entry behaves as a pre-M9
    // entry (warn-but-don't-block on startup). This keeps the
    // existing `mur agent mcp add foo --command not-yet-installed`
    // workflow alive for users who add the entry before installing
    // the binary.
    let (binary_sha256, resolved_path) = match crate::cmd::agent_mcp_pin::resolve_command(command) {
        Ok(p) => match crate::cmd::agent_mcp_pin::compute_binary_sha256(&p) {
            Ok(h) => (Some(h), Some(p)),
            Err(e) => {
                eprintln!(
                    "warning: could not hash {} ({e}); entry will be installed without binary pin",
                    p.display(),
                );
                (None, Some(p))
            }
        },
        Err(_) => {
            eprintln!(
                "warning: could not resolve `{command}` on PATH; entry will be installed without binary pin",
            );
            (None, None)
        }
    };

    let publisher = pin
        .publisher_name
        .as_ref()
        .map(|n| mur_common::agent::McpPublisherInfo {
            name: n.clone(),
            homepage: pin.publisher_homepage.clone(),
            registry_id: pin.publisher_registry_id.clone(),
        });

    if !pin.force {
        // Render summary + prompt.
        println!("About to install MCP server \"{server_id}\":");
        if let Some(p) = &resolved_path {
            println!("  command:        {}", p.display());
        } else {
            println!("  command:        {command} (not yet on PATH)");
        }
        if !args.is_empty() {
            println!("  args:           {}", args.join(" "));
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
        if let Some(h) = &binary_sha256 {
            // Show a short prefix so the user can spot-check against
            // the publisher's release notes.
            println!("  binary sha256:  {}…  (full: {h})", &h[..16]);
        }
        println!(
            "  description hash: <deferred to live MCP probe — will be set on first run via M9.3>",
        );
        print!("\nApprove? [y/N] ");
        use std::io::{self, Write};
        io::stdout().flush().ok();
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .with_context(|| "read confirmation from stdin")?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            bail!("install cancelled");
        }
    }

    profile.mcp_servers.push(McpServerEntry {
        name: server_id.to_string(),
        command: command.to_string(),
        args: args.to_vec(),
        binary_sha256,
        // description_hash is populated by the runtime supervisor on
        // first successful spawn (M9.3). Leaving it `None` here keeps
        // the entry in "warn but don't block" mode until then.
        description_hash: None,
        publisher,
        installed_at: Some(chrono::Utc::now()),
        timeout_secs: None,
        network: None,
        url: None,
        auth: None,
        requires_programs: Vec::new(),
    });
    // Sync spawn allowlist so the supervisor is permitted to launch this MCP.
    if !profile
        .entitlements
        .processes
        .spawn
        .allowed
        .iter()
        .any(|a| a == command)
    {
        profile
            .entitlements
            .processes
            .spawn
            .allowed
            .push(command.to_string());
    }
    save_profile(&path, &mut profile)
}

pub fn cmd_mcp_remove(name: &str, server_id: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let before = profile.mcp_servers.len();
    let removed_command = profile
        .mcp_servers
        .iter()
        .find(|s| s.name == server_id)
        .map(|s| s.command.clone());
    profile.mcp_servers.retain(|s| s.name != server_id);
    if profile.mcp_servers.len() == before {
        bail!("MCP server '{server_id}' not found on '{name}'");
    }
    // Drop the command from the spawn allowlist only if no other mcp entry
    // still needs it.
    if let Some(cmd) = removed_command
        && !profile.mcp_servers.iter().any(|s| s.command == cmd)
    {
        profile
            .entitlements
            .processes
            .spawn
            .allowed
            .retain(|a| a != &cmd);
    }
    save_profile(&path, &mut profile)
}

pub fn cmd_mcp_rename(name: &str, old: &str, new: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if profile.mcp_servers.iter().any(|s| s.name == new) {
        bail!("MCP server '{new}' already exists on '{name}'");
    }
    let hit = profile.mcp_servers.iter_mut().find(|s| s.name == old);
    match hit {
        Some(s) => s.name = new.to_string(),
        None => bail!("MCP server '{old}' not found on '{name}'"),
    }
    save_profile(&path, &mut profile)
}

/// Enable/disable an MCP server for an agent by editing the per-agent
/// denylist. Non-destructive: the entry (and its pin) stays in the profile.
pub fn cmd_mcp_set_enabled(name: &str, server_id: &str, enabled: bool) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if !profile.mcp_servers.iter().any(|s| s.name == server_id) {
        bail!("MCP server '{server_id}' not found on '{name}'");
    }
    profile.set_mcp_enabled(server_id, enabled);
    save_profile(&path, &mut profile)?;
    println!(
        "{} MCP server '{server_id}' for '{name}' (restart the agent to apply)",
        if enabled { "Enabled" } else { "Disabled" }
    );
    Ok(())
}

/// Pure mapping from CLI args to a per-server network policy:
/// `off` ⇒ `Off`; a non-empty allowlist ⇒ `Restricted`; empty + !off ⇒ clear
/// to `None` (inherit the agent-level policy). Non-`BroadAudited` modes never
/// carry an `authorization` record.
fn network_policy_from_args(allow_hosts: Vec<String>, off: bool) -> Option<McpServerNetwork> {
    if off {
        Some(McpServerNetwork {
            mode: McpNetMode::Off,
            allow_hosts: vec![],
            deny_hosts: vec![],
            authorization: None,
        })
    } else if allow_hosts.is_empty() {
        None
    } else {
        Some(McpServerNetwork {
            mode: McpNetMode::Restricted,
            allow_hosts,
            deny_hosts: vec![],
            authorization: None,
        })
    }
}

/// Pure builder for a `BroadAudited` grant: allow-ALL except `deny_hosts`,
/// stamped with who authorized it and when. Requires explicit operator
/// consent upstream (`cmd_mcp_set_network` prompts unless `yes`).
fn broad_audited_network(
    deny_hosts: Vec<String>,
    authorized_by: String,
    authorized_at_ms: u64,
) -> McpServerNetwork {
    McpServerNetwork {
        mode: McpNetMode::BroadAudited,
        allow_hosts: vec![],
        deny_hosts,
        authorization: Some(EgressAuthorization {
            authorized_by,
            authorized_at_ms,
        }),
    }
}

/// Append a `mur.egress.broad_audited.enabled` event to today's trace log,
/// mirroring `skill_curate::record_curation`.
fn record_broad_audited_enabled(
    mur_home: &std::path::Path,
    agent: &str,
    server_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let traces_dir = mur_home.join("traces");
    std::fs::create_dir_all(&traces_dir)
        .with_context(|| format!("create {}", traces_dir.display()))?;
    let path = traces_dir
        .join(now.format("%Y-%m-%d").to_string())
        .with_extension("jsonl");

    let line = serde_json::json!({
        "ts": now.to_rfc3339(),
        "method": METHOD_EGRESS_BROAD_AUDITED_ENABLED,
        "mur.agent.name": agent,
        "mur.mcp.server_id": server_id,
    });

    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    writeln!(f, "{}", serde_json::to_string(&line)?)?;
    Ok(())
}

/// Set (or clear) a per-server egress policy on `server_id`. Restart the agent
/// to apply (the sandbox + proxy are wired at supervisor startup).
///
/// `broad_audited=true` requests an allow-ALL-except-`deny_hosts` grant,
/// routed through the audited egress proxy — a permission-required action.
/// Unless `yes` is set, prompts for explicit `[y/N]` consent on stdin before
/// writing the authorization record and emitting telemetry.
pub fn cmd_mcp_set_network(
    agent: &str,
    server_id: &str,
    allow_hosts: Vec<String>,
    deny_hosts: Vec<String>,
    off: bool,
    broad_audited: bool,
    yes: bool,
) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(agent)?;
    let srv = profile
        .mcp_servers
        .iter_mut()
        .find(|s| s.name == server_id)
        .ok_or_else(|| anyhow::anyhow!("MCP server '{server_id}' not found on '{agent}'"))?;

    if broad_audited {
        println!(
            "This grants MCP server '{server_id}' on agent '{agent}' outbound access to ALL hosts \
             except: {}\nEvery CONNECT is audited, but this is a broad trust grant — only use it \
             for tools that legitimately need unenumerable destinations (e.g. a research browser).",
            if deny_hosts.is_empty() {
                "(none)".to_string()
            } else {
                deny_hosts.join(", ")
            }
        );
        if !yes {
            print!("\nApprove broad-audited egress? [y/N] ");
            use std::io::{self, Write};
            io::stdout().flush().ok();
            let mut answer = String::new();
            io::stdin()
                .read_line(&mut answer)
                .with_context(|| "read confirmation from stdin")?;
            if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                bail!("broad-audited egress grant cancelled");
            }
        }

        let authorized_by = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
        let authorized_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        srv.network = Some(broad_audited_network(
            deny_hosts,
            authorized_by,
            authorized_at_ms,
        ));
        save_profile(&path, &mut profile)?;

        let mur_home = resolve_mur_home()?;
        record_broad_audited_enabled(&mur_home, agent, server_id, chrono::Utc::now())?;
    } else {
        // Any non-BroadAudited mode change clears a prior authorization.
        srv.network = network_policy_from_args(allow_hosts, off);
        save_profile(&path, &mut profile)?;
    }

    println!("Updated egress policy for '{server_id}'. Restart the agent to apply.");
    Ok(())
}

/// Add a remote (Streamable HTTP) MCP server to `agent`. No binary pin — the
/// server runs elsewhere; trust comes from the URL + bearer/OAuth auth.
///
/// `description_hash` pins the tool-schema fingerprint at add-time; if `Some`,
/// it is stored as-is (caller computes via `sha2`).  `egress_host` defaults
/// the server's network policy to `Restricted { allow_hosts: [host] }` so the
/// agent can reach the server's own host without extra configuration.
pub fn cmd_mcp_add_remote(
    agent: &str,
    name: &str,
    url: &str,
    bearer: Option<mur_common::secret::SecretRef>,
    description_hash: Option<String>,
    egress_host: Option<&str>,
) -> anyhow::Result<()> {
    let (path, mut profile) = load_profile_for_edit(agent)?;
    if profile.mcp_servers.iter().any(|m| m.name == name) {
        anyhow::bail!("MCP server '{name}' already exists on '{agent}'; remove it first");
    }
    let network = egress_host.map(|host| mur_common::agent::McpServerNetwork {
        mode: McpNetMode::Restricted,
        allow_hosts: vec![host.to_string()],
        deny_hosts: vec![],
        authorization: None,
    });
    profile.mcp_servers.push(mur_common::agent::McpServerEntry {
        name: name.to_string(),
        url: Some(url.to_string()),
        auth: bearer.map(|token| mur_common::agent::McpAuth::Bearer { token }),
        description_hash,
        network,
        installed_at: Some(chrono::Utc::now()),
        ..Default::default()
    });
    save_profile(&path, &mut profile)?;
    println!("Added remote MCP server '{name}' → {url} for agent '{agent}'.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialize tests that mutate the `MUR_HOME` env var to avoid races.
    static MUR_HOME_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn network_policy_from_args_maps_modes() {
        // Empty + !off → clear (inherit agent policy).
        assert_eq!(network_policy_from_args(vec![], false), None);
        // off → Off, regardless of hosts.
        assert_eq!(
            network_policy_from_args(vec![], true).unwrap().mode,
            McpNetMode::Off
        );
        // Non-empty allowlist → Restricted with those hosts.
        let r = network_policy_from_args(vec!["example.com".into()], false).unwrap();
        assert_eq!(r.mode, McpNetMode::Restricted);
        assert_eq!(r.allow_hosts, vec!["example.com"]);
    }

    #[test]
    fn broad_audited_network_carries_authorization_and_deny_hosts() {
        let net = broad_audited_network(
            vec!["evil.example".into()],
            "dave".into(),
            1_700_000_000_000,
        );
        assert_eq!(net.mode, McpNetMode::BroadAudited);
        assert!(net.allow_hosts.is_empty());
        assert_eq!(net.deny_hosts, vec!["evil.example"]);
        let auth = net.authorization.expect("authorization must be set");
        assert_eq!(auth.authorized_by, "dave");
        assert_eq!(auth.authorized_at_ms, 1_700_000_000_000);
    }

    #[test]
    fn network_policy_from_args_never_carries_authorization() {
        // Non-BroadAudited modes must never carry an authorization record,
        // even if one existed before (mode-change-away-from-BroadAudited
        // clears it — enforced by cmd_mcp_set_network always calling this
        // path when broad_audited=false).
        assert!(
            network_policy_from_args(vec!["example.com".into()], false)
                .unwrap()
                .authorization
                .is_none()
        );
        assert!(
            network_policy_from_args(vec![], true)
                .unwrap()
                .authorization
                .is_none()
        );
    }

    #[test]
    fn record_broad_audited_enabled_appends_an_event() {
        let _lock = MUR_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::TempDir::new().unwrap();
        let mur_home = tmp.path();
        let now = chrono::Utc::now();

        record_broad_audited_enabled(mur_home, "carol", "browser", now).unwrap();

        let path = mur_home
            .join("traces")
            .join(now.format("%Y-%m-%d").to_string())
            .with_extension("jsonl");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains(mur_common::telemetry::METHOD_EGRESS_BROAD_AUDITED_ENABLED));
        assert!(contents.contains("carol"));
        assert!(contents.contains("browser"));
    }

    #[test]
    fn add_with_force_skips_prompt_and_installs() {
        // Regression test for dogfood issue 3: `force = true` must install
        // without touching stdin (the y/N prompt is skipped entirely), and
        // the resulting entry must still carry a real binary_sha256 pin —
        // `force` only bypasses the *consent prompt*, never the hash.
        let _lock = MUR_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::TempDir::new().unwrap();
        let mur_home = tmp.path();

        let agent_home = mur_home.join("agents").join("carol");
        std::fs::create_dir_all(&agent_home).unwrap();
        let p = mur_common::agent::AgentProfile::default_for_tests();
        std::fs::write(
            agent_home.join("profile.yaml"),
            serde_yaml_ng::to_string(&p).unwrap(),
        )
        .unwrap();

        unsafe {
            std::env::set_var("MUR_HOME", mur_home);
        }

        // `true` (the `true` binary, present on every unix box) resolves on
        // PATH, so this also exercises the hash-computation branch.
        cmd_mcp_add(
            "carol",
            "echo-srv",
            "true",
            &[],
            McpAddPin {
                force: true,
                ..Default::default()
            },
        )
        .unwrap();

        let (_p, profile) = load_profile_for_edit("carol").unwrap();
        let e = profile
            .mcp_servers
            .iter()
            .find(|m| m.name == "echo-srv")
            .unwrap();
        assert!(
            e.binary_sha256.is_some(),
            "force must skip only the prompt, not the binary hash pin"
        );
    }

    #[test]
    fn add_remote_writes_url_and_bearer() {
        let _lock = MUR_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::TempDir::new().unwrap();
        let mur_home = tmp.path();

        // Create agent dir with a default profile so load_profile_for_edit works.
        let agent_home = mur_home.join("agents").join("alice");
        std::fs::create_dir_all(&agent_home).unwrap();
        let p = mur_common::agent::AgentProfile::default_for_tests();
        std::fs::write(
            agent_home.join("profile.yaml"),
            serde_yaml_ng::to_string(&p).unwrap(),
        )
        .unwrap();

        unsafe {
            std::env::set_var("MUR_HOME", mur_home);
        }

        cmd_mcp_add_remote(
            "alice",
            "gh",
            "https://api.example.com/mcp",
            Some(mur_common::secret::SecretRef::Env("GH_TOKEN".into())),
            None,
            None,
        )
        .unwrap();

        let (_p, profile) = load_profile_for_edit("alice").unwrap();
        let e = profile.mcp_servers.iter().find(|m| m.name == "gh").unwrap();
        assert_eq!(e.url.as_deref(), Some("https://api.example.com/mcp"));
        assert!(matches!(
            e.auth,
            Some(mur_common::agent::McpAuth::Bearer { .. })
        ));
    }

    #[test]
    fn add_remote_sets_hash_and_default_egress() {
        let _lock = MUR_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::TempDir::new().unwrap();
        let mur_home = tmp.path();

        let agent_home = mur_home.join("agents").join("bob");
        std::fs::create_dir_all(&agent_home).unwrap();
        let p = mur_common::agent::AgentProfile::default_for_tests();
        std::fs::write(
            agent_home.join("profile.yaml"),
            serde_yaml_ng::to_string(&p).unwrap(),
        )
        .unwrap();

        unsafe {
            std::env::set_var("MUR_HOME", mur_home);
        }

        cmd_mcp_add_remote(
            "bob",
            "srv",
            "https://mcp.example.com/mcp",
            None,
            Some("abc123".into()),
            Some("mcp.example.com"),
        )
        .unwrap();

        let (_p, profile) = load_profile_for_edit("bob").unwrap();
        let e = profile
            .mcp_servers
            .iter()
            .find(|m| m.name == "srv")
            .unwrap();
        assert_eq!(e.description_hash.as_deref(), Some("abc123"));
        let net = e.network.as_ref().expect("network should be set");
        assert_eq!(net.mode, McpNetMode::Restricted);
        assert_eq!(net.allow_hosts, vec!["mcp.example.com"]);
    }

    #[test]
    fn render_server_line_broad_audited_with_authorization() {
        let server = McpServerEntry {
            name: "browser".to_string(),
            command: "firefox".to_string(),
            args: vec!["--mcp".to_string()],
            binary_sha256: None,
            description_hash: None,
            publisher: None,
            installed_at: None,
            timeout_secs: None,
            network: Some(McpServerNetwork {
                mode: McpNetMode::BroadAudited,
                allow_hosts: vec![],
                deny_hosts: vec!["badhost.com".to_string()],
                authorization: Some(EgressAuthorization {
                    authorized_by: "alice".to_string(),
                    authorized_at_ms: 1_700_000_000_000,
                }),
            }),
            url: None,
            auth: None,
            requires_programs: Vec::new(),
        };
        let output = render_server_line(&server);
        assert!(output.contains("browser\tfirefox --mcp"));
        assert!(output.contains("BROAD EGRESS (audited)"));
        assert!(output.contains("authorized by alice"));
    }

    #[test]
    fn render_server_line_broad_audited_without_authorization_fallback() {
        let server = McpServerEntry {
            name: "browser".to_string(),
            command: "firefox".to_string(),
            args: vec![],
            binary_sha256: None,
            description_hash: None,
            publisher: None,
            installed_at: None,
            timeout_secs: None,
            network: Some(McpServerNetwork {
                mode: McpNetMode::BroadAudited,
                allow_hosts: vec![],
                deny_hosts: vec![],
                authorization: None,
            }),
            url: None,
            auth: None,
            requires_programs: Vec::new(),
        };
        let output = render_server_line(&server);
        assert!(output.contains("browser\tfirefox"));
        assert!(output.contains("BROAD EGRESS (audited)"));
        assert!(output.contains("authorized by unknown"));
    }

    #[test]
    fn render_server_line_restricted_no_warning() {
        let server = McpServerEntry {
            name: "api".to_string(),
            command: "mcp-api".to_string(),
            args: vec![],
            binary_sha256: None,
            description_hash: None,
            publisher: None,
            installed_at: None,
            timeout_secs: None,
            network: Some(McpServerNetwork {
                mode: McpNetMode::Restricted,
                allow_hosts: vec!["example.com".to_string()],
                deny_hosts: vec![],
                authorization: None,
            }),
            url: None,
            auth: None,
            requires_programs: Vec::new(),
        };
        let output = render_server_line(&server);
        assert!(output.contains("api\tmcp-api"));
        assert!(!output.contains("BROAD EGRESS"));
        assert!(!output.contains("authorized by"));
    }

    #[test]
    fn render_server_line_no_network_no_warning() {
        let server = McpServerEntry {
            name: "local".to_string(),
            command: "local-mcp".to_string(),
            args: vec![],
            binary_sha256: None,
            description_hash: None,
            publisher: None,
            installed_at: None,
            timeout_secs: None,
            network: None,
            url: None,
            auth: None,
            requires_programs: Vec::new(),
        };
        let output = render_server_line(&server);
        assert!(output.contains("local\tlocal-mcp"));
        assert!(!output.contains("BROAD EGRESS"));
        assert!(!output.contains("authorized by"));
    }
}
