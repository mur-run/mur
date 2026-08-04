//! `mur agent perm` — show + mutate the per-agent entitlements section.

use std::fs;

use anyhow::{Context, Result, anyhow, bail};
use mur_common::LockFile;
use mur_common::agent::{NetworkOutboundMode, SpawnMode, ToolPolicy, ToolRule};

use super::{load_profile_for_edit, pid_alive, resolve_mur_home, save_profile};

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
    Ok(())
}

pub fn cmd_perm_set_mode(name: &str, key: &str, value: &str) -> Result<()> {
    match key {
        "network.outbound" => {
            let mode = match value {
                "restricted" => NetworkOutboundMode::Restricted,
                "unrestricted" => NetworkOutboundMode::Unrestricted,
                "off" => NetworkOutboundMode::Off,
                other => bail!("invalid outbound mode '{other}'"),
            };
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

pub fn cmd_perm_allow_host(name: &str, glob: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if !profile
        .entitlements
        .network
        .outbound
        .allow_hosts
        .iter()
        .any(|h| h == glob)
    {
        profile
            .entitlements
            .network
            .outbound
            .allow_hosts
            .push(glob.to_string());
    }
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

pub fn cmd_perm_deny_host(name: &str, glob: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    profile
        .entitlements
        .network
        .outbound
        .allow_hosts
        .retain(|h| h != glob);
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

pub fn cmd_perm_list_hosts(name: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    println!(
        "# allow_hosts ({:?})",
        profile.entitlements.network.outbound.mode
    );
    for h in &profile.entitlements.network.outbound.allow_hosts {
        println!("{h}");
    }
    Ok(())
}

pub fn cmd_perm_allow_read(name: &str, path_arg: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if !profile
        .entitlements
        .filesystem
        .read
        .iter()
        .any(|p| p == path_arg)
    {
        profile
            .entitlements
            .filesystem
            .read
            .push(path_arg.to_string());
    }
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

pub fn cmd_perm_allow_write(name: &str, path_arg: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if !profile
        .entitlements
        .filesystem
        .write
        .iter()
        .any(|p| p == path_arg)
    {
        profile
            .entitlements
            .filesystem
            .write
            .push(path_arg.to_string());
    }
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

pub fn cmd_perm_deny_path(name: &str, path_arg: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if !profile
        .entitlements
        .filesystem
        .deny
        .iter()
        .any(|p| p == path_arg)
    {
        profile
            .entitlements
            .filesystem
            .deny
            .push(path_arg.to_string());
    }
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

pub fn cmd_perm_allow_spawn(name: &str, binary: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if !profile
        .entitlements
        .processes
        .spawn
        .allowed
        .iter()
        .any(|b| b == binary)
    {
        profile
            .entitlements
            .processes
            .spawn
            .allowed
            .push(binary.to_string());
    }
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

pub fn cmd_perm_deny_spawn(name: &str, binary: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    profile
        .entitlements
        .processes
        .spawn
        .allowed
        .retain(|b| b != binary);
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
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

pub fn cmd_perm_set_tool(name: &str, policy: ToolPolicy, pattern: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let rules = &mut profile.entitlements.tools;
    if let Some(r) = rules.iter_mut().find(|r| r.pattern == pattern) {
        r.policy = policy;
    } else {
        rules.push(ToolRule {
            pattern: pattern.to_string(),
            policy,
            risk: None,
        });
    }
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

pub fn cmd_perm_clear_tool(name: &str, pattern: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    profile.entitlements.tools.retain(|r| r.pattern != pattern);
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

pub fn cmd_perm_list_tools(name: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    let rules = &profile.entitlements.tools;
    if rules.is_empty() {
        println!("(no tool rules — all tools use default policy: ask)");
    } else {
        for r in rules {
            println!(
                "{:10}  {}",
                format!("{:?}", r.policy).to_lowercase(),
                r.pattern
            );
        }
    }
    Ok(())
}
