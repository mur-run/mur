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

/// When a profile write reaches the running agent: the tense-free half of
/// the preview's last line. Callers pick it; nothing rewrites rendered text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Applies {
    AfterRestart,
    NextTurn,
    Immediately,
}

impl Applies {
    fn clause(self) -> &'static str {
        match self {
            Applies::AfterRestart => "; takes effect after the agent restarts.",
            Applies::NextTurn => "; the agent's next turn uses it.",
            Applies::Immediately => " and applied to the running agent immediately.",
        }
    }
}

/// The tail for a preview shown BEFORE the write (#2406): future tense,
/// because the user can still decline.
pub(crate) fn will_save_line(a: Applies) -> String {
    format!("Will be saved to the profile{}", a.clause())
}

/// The tail once the write has actually happened.
pub(crate) fn saved_line(a: Applies) -> String {
    format!("Saved to the profile{}", a.clause())
}

/// Render the before → after picture of one MCP server's network policy.
/// No tail: the caller appends [`will_save_line`] or [`saved_line`], since
/// only it knows whether the write has happened and when it applies.
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
    // Lifting a block is a grant too: show every deny-list change (#1919).
    let old_deny: &[String] = before.map(|b| b.deny_hosts.as_slice()).unwrap_or(&[]);
    if !old_deny.is_empty() || !after.deny_hosts.is_empty() {
        out.push("deny list:".into());
        out.extend(host_diff(old_deny, &after.deny_hosts, &[]));
    }
    if research {
        out.push(research_caveat(enforcement).to_owned());
        out.push(
            "link-local and cloud-metadata IPs are still refused by the proxy; \
             loopback and private (LAN) addresses are NOT blocked."
                .into(),
        );
    }
    out.join("\n")
}

/// "signed in as …" for one or several bound profiles: the same wording
/// whether a run binds them or a flagless rerun keeps them (#2545).
pub(super) fn signed_in_as(sites: &[String]) -> String {
    match sites {
        [site] => format!("signed in as the saved profile `{site}`"),
        many => format!(
            "signed in as the saved profiles {} (merged into one session)",
            many.iter()
                .map(|s| format!("`{s}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// The session note(s) for bound profiles (#2551): `signed in as` only for
/// profiles that still have saved state; each missing one gets the repair
/// instead, since the next browser start would fail to load it. `keep` is a
/// flagless rerun, which also tells the user how to unbind (#2545) — once,
/// on the last note. A site whose state is unknown (`None`) gets a neutral
/// "could not check" note: neither "signed in" nor the repair would be
/// grounded in anything.
pub(super) fn session_notes(
    sites: &[String],
    keep: bool,
    mut is_saved: impl FnMut(&str) -> Option<bool>,
) -> Vec<String> {
    let mut saved = Vec::new();
    let mut missing = Vec::new();
    let mut unknown = Vec::new();
    for site in sites {
        match is_saved(site) {
            Some(true) => saved.push(site.clone()),
            Some(false) => missing.push(site.clone()),
            None => unknown.push(site.clone()),
        }
    }
    let mut notes = Vec::new();
    if !saved.is_empty() {
        let line = signed_in_as(&saved);
        notes.push(if keep { format!("still {line}") } else { line });
    }
    notes.extend(missing.iter().map(|site| {
        format!(
            "profile `{site}` is bound but not saved; run `mur browser auth {site} --url <login url>` \
             or rerun with `--no-profile`"
        )
    }));
    // State unknown (MUR's home did not resolve): claim neither "signed in"
    // nor "not saved", but do not go silent either — the unbind hint for a
    // kept binding would otherwise vanish with it.
    if !unknown.is_empty() {
        let noun = if unknown.len() == 1 {
            "profile"
        } else {
            "profiles"
        };
        let names = unknown
            .iter()
            .map(|s| format!("`{s}`"))
            .collect::<Vec<_>>()
            .join(", ");
        notes.push(format!(
            "could not check {noun} {names}: MUR's home did not resolve"
        ));
    }
    // One unbind hint per kept binding, on the last note — unless that note
    // is a repair line, which already offers `--no-profile`.
    if keep
        && let Some(last) = notes.last_mut()
        && !last.contains("`--no-profile`")
    {
        last.push_str("; rerun with `--no-profile` to unbind");
    }
    notes
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
    fn tails_differ_only_in_tense() {
        for a in [
            Applies::AfterRestart,
            Applies::NextTurn,
            Applies::Immediately,
        ] {
            let (will, done) = (will_save_line(a), saved_line(a));
            assert!(will.starts_with("Will be saved to the profile"), "{will}");
            assert!(done.starts_with("Saved to the profile"), "{done}");
            assert_eq!(
                will.trim_start_matches("Will be saved"),
                done.trim_start_matches("Saved")
            );
        }
        assert!(will_save_line(Applies::AfterRestart).contains("after the agent restarts"));
        assert!(will_save_line(Applies::Immediately).contains("running agent immediately"));
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
        assert!(p.contains("agent `mur`"), "{p}");
        // The tail is the caller's: tense depends on whether it wrote (#2406).
        assert!(!p.contains("Saved") && !p.contains("Will be saved"), "{p}");
        assert!(!p.contains("research"), "{p}");
    }

    #[test]
    fn preview_shows_deny_hosts_being_lifted() {
        let before = McpServerNetwork {
            mode: McpNetMode::BroadAudited,
            deny_hosts: vec!["evil.example".into(), "keep.example".into()],
            ..Default::default()
        };
        let after = McpServerNetwork {
            mode: McpNetMode::BroadAudited,
            deny_hosts: vec!["keep.example".into()],
            ..Default::default()
        };
        let p = render_preview(
            "mur",
            "browser",
            Some(&before),
            &after,
            &[],
            DenyEnforcement::Advisory,
        );
        assert!(p.contains("- evil.example"), "{p}");
        assert!(p.contains("  keep.example"), "{p}");
        assert!(p.contains("deny list"), "{p}");
    }

    #[test]
    fn preview_shows_deny_list_dropped_when_leaving_research() {
        let before = McpServerNetwork {
            mode: McpNetMode::BroadAudited,
            deny_hosts: vec!["evil.example".into()],
            ..Default::default()
        };
        let after = net(McpNetMode::Restricted, &["a.com"]);
        let p = render_preview(
            "mur",
            "browser",
            Some(&before),
            &after,
            &[],
            DenyEnforcement::Advisory,
        );
        assert!(p.contains("- evil.example"), "{p}");
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
            p.contains("link-local and cloud-metadata IPs are still refused")
                && p.contains("loopback and private (LAN) addresses are NOT blocked"),
            "{p}"
        );
    }

    /// An unknown save state (MUR's home does not resolve) yields one neutral
    /// "could not check" note: neither "signed in" nor the `mur browser auth`
    /// repair, which would blame a profile that may be fine. Known sites in
    /// the same run keep their notes, and a kept binding gets the unbind hint
    /// exactly once.
    #[test]
    fn session_notes_say_unknown_state_neutrally() {
        let sites = |s: &[&str]| s.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();

        let fresh = session_notes(&sites(&["a", "b"]), false, |_| None);
        assert_eq!(
            fresh,
            ["could not check profiles `a`, `b`: MUR's home did not resolve"],
        );
        let kept = session_notes(&sites(&["a"]), true, |_| None);
        assert_eq!(
            kept,
            ["could not check profile `a`: MUR's home did not resolve; \
              rerun with `--no-profile` to unbind"],
        );
        let kept_many = session_notes(&sites(&["a", "b"]), true, |_| None);
        assert_eq!(
            kept_many,
            [
                "could not check profiles `a`, `b`: MUR's home did not resolve; \
              rerun with `--no-profile` to unbind"
            ],
        );
        for n in fresh.iter().chain(&kept).chain(&kept_many) {
            assert!(!n.contains("signed in"), "{n}");
            assert!(!n.contains("mur browser auth"), "{n}");
        }

        let notes = session_notes(&sites(&["a", "b", "c"]), false, |s| match s {
            "a" => Some(true),
            "b" => Some(false),
            _ => None,
        });
        assert_eq!(notes.len(), 3, "{notes:?}");
        assert_eq!(notes[0], signed_in_as(&sites(&["a"])));
        assert!(
            notes[1].contains("profile `b` is bound but not saved"),
            "{notes:?}"
        );
        assert!(
            notes[2].starts_with("could not check profile `c`:"),
            "{notes:?}"
        );
        assert!(!notes[..2].iter().any(|n| n.contains("`c`")), "{notes:?}");

        // Flagless rerun, saved + unknown: the unbind hint appears once, on
        // the last line, not on every line.
        let mixed = session_notes(&sites(&["a", "c"]), true, |s| (s == "a").then_some(true));
        assert_eq!(
            mixed,
            [
                "still signed in as the saved profile `a`".to_owned(),
                "could not check profile `c`: MUR's home did not resolve; \
                 rerun with `--no-profile` to unbind"
                    .to_owned(),
            ],
        );
        let hint = |ns: &[String]| {
            ns.iter()
                .map(|n| n.matches("`--no-profile`").count())
                .sum::<usize>()
        };
        assert_eq!(hint(&mixed), 1, "{mixed:?}");
        // Saved + missing: the repair line already offers `--no-profile`.
        let repair = session_notes(&sites(&["a", "b"]), true, |s| Some(s == "a"));
        assert_eq!(hint(&repair), 1, "{repair:?}");
        assert_eq!(repair[0], "still signed in as the saved profile `a`");
    }
}
