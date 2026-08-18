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
    validate_host_pattern(name, glob)?;
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

/// Refuse patterns the matcher can never match, at write time.
///
/// `mur_common::net::host_matches_pattern` compares PORTLESS hosts — exact,
/// `*.suffix`, or legacy `.suffix`. Anything else written into `allow_hosts`
/// is silently inert: it round-trips through `list-hosts` looking configured
/// while matching nothing (field report: `allow-host 'IP:3306'` accepted, the
/// connection still blocked, no warning anywhere). `deny-host` is left
/// unvalidated on purpose — it must be able to REMOVE junk entries.
fn validate_host_pattern(name: &str, glob: &str) -> Result<()> {
    let g = glob.trim();
    if g.is_empty() {
        bail!("empty host pattern");
    }
    if g.contains("://") || g.contains('/') || g.contains(char::is_whitespace) {
        bail!(
            "'{glob}' is not a hostname — pass a bare host (`api.example.com`), a wildcard (`*.example.com`), or an IP"
        );
    }
    // host:port (incl. `[v6]:port`). A per-host PORT is not expressible
    // today: the OS sandbox restricts by port only (host stays `*`), and
    // every allow_hosts consumer strips the port before matching. Refuse
    // rather than write a rule nothing reads.
    let single_colon_port = g.bytes().filter(|&b| b == b':').count() == 1
        && g.rsplit_once(':')
            .is_some_and(|(_, p)| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    if g.starts_with('[') || single_colon_port {
        let host = g.rsplit_once(':').map(|(h, _)| h).unwrap_or(g);
        let ports = mur_agent_runtime::sandbox::policy::RESTRICTED_GENERAL_PORTS
            .map(|p| p.to_string())
            .join("/");
        bail!(
            "'{glob}' looks like host:port, which would have NO effect — allow_hosts matches hostnames only, and in `restricted` mode the sandbox opens ports {ports} to any host regardless.\n\
             To reach {host} on a non-web port today: `mur agent perm set-mode {name} unrestricted` (opens all ports).\n\
             To allow the HOST for web traffic: `mur agent perm allow-host {name} {host}`"
        );
    }
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

/// Refuse a filesystem grant the sandbox would silently discard.
///
/// `SandboxPolicy::from_entitlements` drops entitlement paths that do not exist
/// when the profile is sealed (Issue 16 — a dead grant destabilizes unrelated
/// write checks). Accepting one here is the worst outcome: the CLI reports
/// success, the profile lists the path, `restart` says it applied, and the
/// kernel still returns EPERM with nothing in between explaining why.
fn reject_dead_grant(path_arg: &str) -> Result<()> {
    let p = mur_agent_runtime::sandbox::policy::expand_entitlement_path(path_arg);
    if std::fs::metadata(&p).is_ok() {
        return Ok(());
    }
    anyhow::bail!(
        "{} does not exist.\n\
         The sandbox drops grants for paths that are missing when the agent \
         starts, so this one would be accepted here and still denied by the \
         kernel. Create it first, then re-run this command:\n    mkdir -p {}",
        p.display(),
        p.display()
    )
}

/// Refuse a grant that no sandbox would honour, before it reaches the profile.
///
/// Distinct from `reject_dead_grant`, which refuses a path that does not exist
/// yet. This one refuses paths that must never be granted at all.
fn reject_ungrantable(name: &str, path_arg: &str, write: bool) -> Result<()> {
    use mur_agent_runtime::sandbox::launch_chain::{LaunchChain, is_overbroad_grant_root};

    let p = mur_agent_runtime::sandbox::policy::expand_entitlement_path(path_arg);
    let agent_home = super::resolve_mur_home()?.join("agents").join(name);
    let chain = LaunchChain::new(&agent_home);

    let hit = if write {
        chain.protects_write(&p)
    } else {
        chain.protects_read(&p)
    };
    if let Some(reason) = hit {
        anyhow::bail!(
            "{} is part of MUR's launch chain and can never be granted: {reason}",
            p.display()
        );
    }

    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));
    if is_overbroad_grant_root(&p, &home) {
        anyhow::bail!(
            "{} is too broad to grant — it covers the whole machine, the whole \
             home directory, or a volume root. Grant the specific project dir instead.",
            p.display()
        );
    }
    Ok(())
}

pub fn cmd_perm_allow_read(name: &str, path_arg: &str) -> Result<()> {
    reject_ungrantable(name, path_arg, false)?;
    reject_dead_grant(path_arg)?;
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
    reject_ungrantable(name, path_arg, true)?;
    reject_dead_grant(path_arg)?;
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

/// Grant the build lane: every executable under `dir` becomes spawnable.
///
/// The binary allowlist cannot express a toolchain that compiles its own
/// executables — a Rust build execs `target/debug/build/<crate>-<hash>/
/// build-script-build`, proc-macro shims and freshly linked test binaries,
/// at paths that do not exist until the build creates them. Print what the
/// grant means rather than accepting it silently: this is a wider door than
/// naming one binary, and the operator should see that in the terminal.
pub fn cmd_perm_allow_spawn_dir(name: &str, dir: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let dirs = &mut profile.entitlements.processes.spawn.allowed_dirs;
    if !dirs.iter().any(|d| d == dir) {
        dirs.push(dir.to_string());
    }
    save_profile(&path, &mut profile)?;
    println!("build lane: '{name}' may now exec anything under {dir}");
    println!(
        "  filesystem and network entitlements still bound what that code can reach — \
         check them with `mur agent perm show {name}`"
    );
    warn_if_running(name);
    Ok(())
}

pub fn cmd_perm_deny_spawn_dir(name: &str, dir: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    profile
        .entitlements
        .processes
        .spawn
        .allowed_dirs
        .retain(|d| d != dir);
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

#[cfg(test)]
mod tests {
    use super::validate_host_pattern;

    #[test]
    fn patterns_the_matcher_can_match_are_accepted() {
        for ok in [
            "api.example.com",
            "*.example.com",
            ".example.com",
            "10.0.0.5",
            "2001:db8::1", // bare IPv6: multiple colons, not host:port
        ] {
            assert!(validate_host_pattern("a1", ok).is_ok(), "{ok} must pass");
        }
    }

    #[test]
    fn inert_patterns_are_refused_with_guidance() {
        for bad in [
            "35.229.166.236:3306",
            "example.com:443",
            "[::1]:8080",
            "https://example.com",
            "example.com/path",
            "two hosts",
            "",
        ] {
            assert!(
                validate_host_pattern("a1", bad).is_err(),
                "{bad:?} must be refused"
            );
        }
        // The field-report case names the real remedy, with the agent's name.
        let err = validate_host_pattern("data-ml", "35.229.166.236:3306")
            .unwrap_err()
            .to_string();
        assert!(err.contains("NO effect"), "{err}");
        assert!(err.contains("set-mode data-ml unrestricted"), "{err}");
    }
}
