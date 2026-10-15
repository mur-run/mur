//! What `/browser live` is about to write, shown before it is written (#1905).

use mur_agent_runtime::sandbox::egress_proxy::DenyEnforcement;
use mur_common::agent::{McpNetMode, McpServerNetwork};

/// The research-mode caveat, generated from the proxy's real enforcement
/// state so the text cannot drift from what the proxy does (D1).
pub(crate) fn research_caveat(e: DenyEnforcement) -> &'static str {
    match e {
        DenyEnforcement::Advisory => {
            "research mode: any host is reachable and every connection is logged; \
             deny-list entries are only logged, not blocked."
        }
        DenyEnforcement::Enforced => {
            "research mode: any host is reachable and every connection is logged; \
             deny-list hosts are blocked."
        }
    }
}

/// `+ added`, `- removed`, `  kept` (in `after` order, removed last), then
/// one `(input)` line per normalization note. Shared by every egress writer.
pub(crate) fn host_diff(before: &[String], after: &[String], notes: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for h in after {
        let mark = if before.contains(h) { ' ' } else { '+' };
        out.push(format!("{mark} {h}"));
    }
    for h in before.iter().filter(|h| !after.contains(h)) {
        out.push(format!("- {h}"));
    }
    for n in notes {
        out.push(format!("  (input) {n}"));
    }
    out
}

/// Ask `Save? [y/N]` on stderr when a person is at the terminal and did not
/// pass `--yes`. Scripts, the wizard and hub-gui (non-TTY) write without a
/// prompt, as before; the stderr preview is their record.
pub(crate) fn confirm_on_tty(yes: bool) -> anyhow::Result<()> {
    use std::io::{IsTerminal, Write};
    if yes || !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Ok(());
    }
    eprint!("Save? [y/N] ");
    std::io::stderr().flush().ok();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|e| anyhow::anyhow!("read confirmation from stdin: {e}"))?;
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        anyhow::bail!("cancelled — nothing was changed");
    }
    Ok(())
}

/// Render the before → after picture of one MCP server's network policy.
pub(crate) fn render_preview(
    agent: &str,
    server: &str,
    before: Option<&McpServerNetwork>,
    after: &McpServerNetwork,
    notes: &[String],
    enforcement: DenyEnforcement,
) -> String {
    let research = after.mode == McpNetMode::BroadAudited;
    let what = if research {
        "research mode (all hosts, audited)"
    } else {
        "only the hosts listed"
    };
    let mut out = vec![format!("`{server}` on agent `{agent}` will be: {what}")];
    let old: &[String] = before.map(|b| b.allow_hosts.as_slice()).unwrap_or(&[]);
    out.extend(host_diff(old, &after.allow_hosts, notes));
    if research {
        if !after.deny_hosts.is_empty() {
            out.push(format!("deny list: {}", after.deny_hosts.join(", ")));
        }
        out.push(research_caveat(enforcement).to_owned());
        out.push(
            "private, link-local and cloud-metadata IPs are still refused by the proxy.".into(),
        );
    }
    out.push("Saved to the profile; takes effect after the agent restarts.".into());
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(mode: McpNetMode, hosts: &[&str]) -> McpServerNetwork {
        McpServerNetwork {
            mode,
            allow_hosts: hosts.iter().map(|h| (*h).to_owned()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn caveat_follows_enforcement() {
        assert!(research_caveat(DenyEnforcement::Advisory).contains("only logged, not blocked"));
        assert!(research_caveat(DenyEnforcement::Enforced).contains("are blocked"));
    }

    #[test]
    fn restricted_preview_shows_diff_and_notes() {
        let before = net(McpNetMode::Restricted, &["old.com", "keep.com"]);
        let after = net(McpNetMode::Restricted, &["keep.com", "new.com"]);
        let p = render_preview(
            "mur",
            "browser",
            Some(&before),
            &after,
            &["`https://new.com/x` → `new.com`".into()],
            DenyEnforcement::Advisory,
        );
        assert!(
            p.contains("+ new.com") && p.contains("- old.com") && p.contains("  keep.com"),
            "{p}"
        );
        assert!(p.contains("https://new.com/x"), "{p}");
        assert!(p.contains("agent `mur`") && p.contains("restart"), "{p}");
        assert!(!p.contains("research"), "{p}");
    }

    #[test]
    fn research_preview_carries_generated_caveat_and_ssrf_line() {
        let after = McpServerNetwork {
            mode: McpNetMode::BroadAudited,
            ..Default::default()
        };
        let p = render_preview(
            "mur",
            "browser",
            None,
            &after,
            &[],
            DenyEnforcement::Advisory,
        );
        assert!(
            p.contains(research_caveat(DenyEnforcement::Advisory)),
            "{p}"
        );
        assert!(
            p.contains("private, link-local and cloud-metadata IPs are still refused"),
            "{p}"
        );
    }
}
