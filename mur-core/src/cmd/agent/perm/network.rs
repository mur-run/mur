//! `mur agent perm` network grants: outbound hosts and ports.

use anyhow::{Result, bail};
use mur_common::agent::NetworkOutboundMode;

use super::super::{load_profile_for_edit, save_profile};
use super::{print_outbound_picture, warn_if_running};

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
        let port_hint = g.rsplit_once(':').map(|(_, p)| p).unwrap_or("<port>");
        let ports = mur_agent_runtime::sandbox::policy::RESTRICTED_GENERAL_PORTS
            .map(|p| p.to_string())
            .join("/");
        bail!(
            "'{glob}' looks like host:port, which would have NO effect — allow_hosts matches hostnames only.\n\
             In `restricted` mode the sandbox opens ports {ports} to any host; a non-web port is granted separately.\n\
             To reach {host} on port {port_hint}: `mur agent perm allow-port {name} {port_hint}` (opens that port to any host)\n\
             To allow the HOST for web traffic: `mur agent perm allow-host {name} {host}`"
        );
    }
    Ok(())
}

/// `mur agent perm allow-port <agent> <port>` — grant an extra outbound TCP
/// port under `restricted` (issue #006).
///
/// Deliberately blunt about what it is NOT: the OS sandbox restricts by port
/// with the host left as `*`, so this opens the port to EVERY host, not just
/// the one the user has in mind. Saying so at grant time is the only place
/// the user is still thinking about the decision.
pub fn cmd_perm_allow_port(name: &str, port: u16) -> Result<()> {
    if port == 0 {
        bail!("port 0 is not dialable");
    }
    let (path, mut profile) = load_profile_for_edit(name)?;
    let mode = profile.entitlements.network.outbound.mode;
    if is_base_port(port) {
        println!("port {port} is already open by default in `restricted` mode — nothing to do");
        return Ok(());
    }
    add_port(&mut profile.entitlements.network.outbound.allow_ports, port);
    save_profile(&path, &mut profile)?;

    // The grant is written either way, but under a mode that ignores it the
    // user must not walk away believing the port is open.
    match mode {
        NetworkOutboundMode::Restricted => {
            println!(
                "granted outbound TCP port {port} to '{name}'.\n\
                 NOTE: this opens port {port} to ANY host — the OS sandbox filters by port, not by host.\n\
                 To bound which hosts are reachable: `mur agent perm allow-host {name} <host>`"
            );
        }
        NetworkOutboundMode::Unrestricted => {
            println!(
                "recorded port {port} for '{name}', but outbound mode is `unrestricted` — every port is already open.\n\
                 To make this grant meaningful: `mur agent perm set-mode {name} network.outbound restricted`"
            );
        }
        NetworkOutboundMode::ProxyOnly | NetworkOutboundMode::Off => {
            println!(
                "recorded port {port} for '{name}', but outbound mode is `{}` — general TCP stays denied and this grant has NO effect until the mode is `restricted`.",
                match mode {
                    NetworkOutboundMode::ProxyOnly => "proxy_only",
                    _ => "off",
                }
            );
        }
    }
    warn_if_running(name);
    Ok(())
}

/// `mur agent perm deny-port <agent> <port>` — take an extra port grant back.
pub fn cmd_perm_deny_port(name: &str, port: u16) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if is_base_port(port) {
        bail!(
            "port {port} is part of the built-in web set (80/443/8080/8443) and cannot be removed individually.\n\
             To close general egress entirely: `mur agent perm set-mode {name} network.outbound off`"
        );
    }
    let removed = remove_port(&mut profile.entitlements.network.outbound.allow_ports, port);
    save_profile(&path, &mut profile)?;
    if !removed {
        println!("port {port} was not granted to '{name}' — nothing to do");
    }
    warn_if_running(name);
    Ok(())
}

/// Insert `port`, deduped and sorted. Sorted so the profile diff is stable
/// across grants and two agents with the same grants produce the same file.
pub(crate) fn add_port(ports: &mut Vec<u16>, port: u16) {
    if !ports.contains(&port) {
        ports.push(port);
        ports.sort_unstable();
    }
}

/// Drop `port`; reports whether it was actually there, so the caller can say
/// "nothing to do" rather than implying a grant was revoked.
fn remove_port(ports: &mut Vec<u16>, port: u16) -> bool {
    let before = ports.len();
    ports.retain(|p| *p != port);
    ports.len() != before
}

/// Whether `port` is part of the built-in web set, which `restricted` opens
/// unconditionally and `deny-port` therefore cannot take back.
pub(crate) fn is_base_port(port: u16) -> bool {
    mur_agent_runtime::sandbox::policy::RESTRICTED_GENERAL_PORTS.contains(&port)
}

/// `mur agent perm list-ports <agent>` — every outbound TCP port, base set and
/// user grants together, labelled by whether the current mode actually honors
/// them.
pub fn cmd_perm_list_ports(name: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    let out = &profile.entitlements.network.outbound;
    let extra = &out.allow_ports;
    match out.mode {
        NetworkOutboundMode::Restricted => {
            println!("outbound mode: restricted — these TCP ports are open (to any host):");
            for p in mur_agent_runtime::sandbox::policy::RESTRICTED_GENERAL_PORTS {
                println!("  {p}\t(built-in)");
            }
            for p in extra {
                println!("  {p}\t(granted)");
            }
        }
        NetworkOutboundMode::Unrestricted => {
            println!("outbound mode: unrestricted — ALL ports are open; port grants are moot.");
            if !extra.is_empty() {
                println!("recorded (inert) grants: {extra:?}");
            }
        }
        NetworkOutboundMode::ProxyOnly => {
            println!(
                "outbound mode: proxy_only — general TCP is denied; egress only via loopback proxies."
            );
            if !extra.is_empty() {
                println!("recorded (inert) grants: {extra:?}");
            }
        }
        NetworkOutboundMode::Off => {
            println!("outbound mode: off — all outbound TCP denied.");
            if !extra.is_empty() {
                println!("recorded (inert) grants: {extra:?}");
            }
        }
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
    // The whole picture, not just the agent-level list: printing one of two
    // policies that both call themselves `allow_hosts` is what taught users
    // that this command scopes their MCP servers.
    print_outbound_picture(&profile);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{add_port, is_base_port, remove_port, validate_host_pattern};

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
        // Issue #006: the remedy is a port grant, NOT `unrestricted`. Before
        // extra ports existed this text pushed users to open EVERY port to
        // reach one; asserting on the narrow remedy is what stops that
        // guidance from creeping back.
        assert!(err.contains("allow-port data-ml 3306"), "{err}");
        assert!(
            !err.contains("unrestricted"),
            "must no longer recommend opening all ports: {err}"
        );
    }

    /// Issue #006: grants dedupe and stay sorted, so repeated `allow-port`
    /// cannot emit duplicate sandbox rules or churn the profile diff.
    #[test]
    fn allow_port_dedupes_and_sorts() {
        let mut ports = vec![];
        add_port(&mut ports, 5173);
        add_port(&mut ports, 2222);
        add_port(&mut ports, 5173);
        assert_eq!(ports, vec![2222, 5173]);
    }

    #[test]
    fn deny_port_reports_whether_the_grant_existed() {
        let mut ports = vec![2222, 5173];
        assert!(remove_port(&mut ports, 2222));
        assert_eq!(ports, vec![5173]);
        assert!(
            !remove_port(&mut ports, 2222),
            "already gone — must not claim a revoke"
        );
    }

    /// A built-in web port cannot be revoked individually: `restricted` opens
    /// it unconditionally, so removing it from `allow_ports` would change
    /// nothing while looking like it closed the port.
    #[test]
    fn base_ports_are_recognized_and_not_individually_revocable() {
        for p in mur_agent_runtime::sandbox::policy::RESTRICTED_GENERAL_PORTS {
            assert!(is_base_port(p), "{p} is part of the built-in web set");
        }
        assert!(!is_base_port(2222));
        assert!(!is_base_port(5173));
    }
}
