//! `mur agent perm` — show + mutate the per-agent entitlements section.

use std::fs;

use anyhow::{Context, Result, anyhow, bail};
use mur_common::LockFile;
use mur_common::agent::{NetworkOutboundMode, SpawnMode};

use super::{load_profile_for_edit, pid_alive, resolve_mur_home, save_profile};

mod network;
mod paths;
mod spawn;
mod tools;

pub use network::{
    cmd_perm_allow_host, cmd_perm_allow_port, cmd_perm_deny_host, cmd_perm_deny_port,
    cmd_perm_list_hosts, cmd_perm_list_ports,
};
pub(crate) use paths::reject_ungrantable_path;
pub use paths::{
    cmd_perm_allow_read, cmd_perm_allow_write, cmd_perm_deny_path, cmd_perm_list_paths,
    cmd_perm_remove_path,
};
pub use spawn::{
    cmd_perm_allow_spawn, cmd_perm_allow_spawn_dir, cmd_perm_deny_spawn, cmd_perm_deny_spawn_dir,
};
pub use tools::{cmd_perm_clear_tool, cmd_perm_list_tools, cmd_perm_set_tool};

/// Emit a stderr warning when changing perms on a running agent. Wraps
/// the lock-file probe so callers don't need to know the file layout.
pub(super) fn warn_if_running(name: &str) {
    let mur_home = match resolve_mur_home() {
        Ok(p) => p,
        Err(_) => return,
    };
    let lock_path = mur_home.join("agents").join(name).join("running.lock");
    if !lock_path.exists() {
        return;
    }
    let bytes = match fs::read(&lock_path) {
        Ok(b) => b,
        Err(_) => return,
    };
    if let Ok(lock) = serde_json::from_slice::<LockFile>(&bytes)
        && pid_alive(lock.pid)
    {
        eprintln!("warning: '{name}' is running; restart required for changes to take effect");
        eprintln!("         run: mur agent restart {name}");
    }
}

/// Render an agent's COMPLETE outbound picture: what the runtime itself may
/// reach, and what each MCP server may reach.
///
/// These are different subjects enforced by different machinery, and showing
/// only one of them is how a user comes to believe `perm allow-host` scopes
/// their MCP servers. It does not: that list is an in-process guard on the
/// runtime's own HTTP client (plus the B0 gate on the agent's `network.*`
/// tools), and a spawned server runs neither.
fn print_outbound_picture(profile: &mur_common::AgentProfile) {
    print!("{}", super::perm_view::outbound_picture(profile));
}

pub fn cmd_perm_show(name: &str, section: Option<&str>) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    let v = serde_yaml_ng::to_string(&profile.entitlements).context("serialize entitlements")?;
    if let Some(sec) = section {
        // Print only the requested top-level YAML section if present.
        let mut emit = false;
        for line in v.lines() {
            if let Some(rest) = line.strip_prefix(&format!("{sec}:")) {
                println!("{sec}:{rest}");
                emit = true;
                continue;
            }
            if emit {
                if line.starts_with(' ') || line.is_empty() {
                    println!("{line}");
                } else {
                    break;
                }
            }
        }
    } else {
        print!("{v}");
    }
    if !profile.mcp_servers.is_empty() {
        eprintln!();
        eprintln!("# outbound scope (`mur agent perm list-hosts {name}` for detail)");
        eprintln!(
            "# allow_hosts above governs the RUNTIME only; the {} MCP server(s) \
             have their own policy.",
            profile.mcp_servers.len()
        );
    }
    Ok(())
}

/// The wire names plus the two spellings people type for the fourth mode.
/// `ProxyOnly` was unreachable from the CLI until now.
fn parse_outbound_mode(value: &str) -> Result<NetworkOutboundMode> {
    Ok(match value {
        "restricted" => NetworkOutboundMode::Restricted,
        "unrestricted" => NetworkOutboundMode::Unrestricted,
        "proxy_only" | "proxy-only" | "proxyonly" => NetworkOutboundMode::ProxyOnly,
        "off" => NetworkOutboundMode::Off,
        other => {
            bail!("invalid outbound mode '{other}' (restricted, unrestricted, proxy_only, off)")
        }
    })
}

pub fn cmd_perm_set_mode(name: &str, key: &str, value: &str) -> Result<()> {
    match key {
        "network.outbound" => {
            let mode = parse_outbound_mode(value)?;
            let (path, mut profile) = load_profile_for_edit(name)?;
            profile.entitlements.network.outbound.mode = mode;
            save_profile(&path, &mut profile)?;
            warn_if_running(name);
            Ok(())
        }
        "processes.spawn" => {
            let mode = match value {
                "strict" => SpawnMode::Strict,
                "allowlist" => SpawnMode::Allowlist,
                "any" => SpawnMode::Any,
                "none" => SpawnMode::None,
                other => bail!("invalid spawn mode '{other}'"),
            };
            let (path, mut profile) = load_profile_for_edit(name)?;
            profile.entitlements.processes.spawn.mode = mode;
            save_profile(&path, &mut profile)?;
            warn_if_running(name);
            Ok(())
        }
        other => bail!(
            "set-mode: unsupported key '{other}' (valid keys: network.outbound, processes.spawn)"
        ),
    }
}

pub fn cmd_perm_set_limit(name: &str, key: &str, value: u64) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let lim = &mut profile.entitlements.limits;
    match key {
        "memory_mb" => lim.memory_mb = value,
        "file_descriptors" => {
            lim.file_descriptors = u32::try_from(value)
                .map_err(|_| anyhow!("file_descriptors out of range for u32"))?
        }
        "processes" => {
            lim.processes =
                u32::try_from(value).map_err(|_| anyhow!("processes out of range for u32"))?
        }
        other => bail!("set-limit: unsupported key '{other}'"),
    }
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

/// `mur agent perm reseal <name>` — trust the entitlements now on disk (#712).
pub fn cmd_perm_reseal(name: &str) -> Result<()> {
    use mur_common::entitlements_pin::{self, PinCheck};
    let mur_home = resolve_mur_home()?;
    let path = mur_home.join("agents").join(name).join("profile.yaml");
    let yaml = fs::read_to_string(&path).map_err(|_| anyhow!("agent '{name}' not found"))?;
    let ent = entitlements_pin::entitlements_from_yaml(&yaml, &path)?;
    match entitlements_pin::check(&mur_home, name, &ent) {
        Ok(PinCheck::Match) => {
            println!("{name}: entitlements already match the pin; nothing to reseal");
            return Ok(());
        }
        Ok(PinCheck::Mismatch { changed }) => {
            println!(
                "{name}: accepting changed entitlements: {}",
                changed.join(", ")
            );
        }
        Ok(PinCheck::Missing) | Err(_) => println!("{name}: pinning current entitlements"),
    }
    entitlements_pin::write_pin(&mur_home, name, &ent)?;
    println!("Resealed. Review with `mur agent perm {name}`; restart the agent to apply.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_outbound_mode;

    #[test]
    fn proxy_only_is_now_reachable_from_the_cli() {
        use mur_common::agent::NetworkOutboundMode as M;
        assert_eq!(parse_outbound_mode("proxy_only").unwrap(), M::ProxyOnly);
        assert_eq!(parse_outbound_mode("proxy-only").unwrap(), M::ProxyOnly);
        assert_eq!(parse_outbound_mode("off").unwrap(), M::Off);
        assert!(parse_outbound_mode("open").is_err());
    }
}
