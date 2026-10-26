//! When a `/browser live` write takes effect, and what the reply says about it.
//!
//! An edit to the existing `browser` entry (hosts, `--profile`) is hot: the
//! running agent starts a new browser client on its next turn and closes the
//! old one (spec 2026-10-25 D1). Creating the entry, a new grant, or switching
//! research mode on or off is cold and still needs a restart (D3, §4).

use mur_common::agent::McpNetMode;

/// When the write reaches the running agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Effect {
    /// Every changed field is hot: the next turn uses it.
    NextTurn,
    /// Something cold changed (or the entry is new): restart the agent.
    Restart,
}

impl Effect {
    /// Classify a profile write the way the running agent will: by the
    /// reload diff between what was on disk and what was written.
    pub(super) fn of_write(
        before: &mur_common::AgentProfile,
        after: &mur_common::AgentProfile,
    ) -> Self {
        if mur_common::AgentProfile::reload_diff(before, after)
            .cold
            .is_empty()
        {
            Effect::NextTurn
        } else {
            Effect::Restart
        }
    }

    /// The preview's guess, before anything is written: hot only for an
    /// existing entry whose mode stays and that needs no repair (a repair adds
    /// grants, which are cold).
    pub(super) fn predicted(
        existing_mode: Option<McpNetMode>,
        after_mode: McpNetMode,
        repairs_pending: bool,
    ) -> Self {
        match existing_mode {
            Some(m) if m == after_mode && !repairs_pending => Effect::NextTurn,
            _ => Effect::Restart,
        }
    }

    /// The preview's last line.
    pub(super) fn preview_line(self) -> &'static str {
        match self {
            Effect::NextTurn => "Saved to the profile; the agent's next turn uses it.",
            Effect::Restart => "Saved to the profile; takes effect after the agent restarts.",
        }
    }

    /// The line after a successful write.
    pub(super) fn applied_line(self) -> &'static str {
        match self {
            Effect::NextTurn => APPLIED_HOT,
            Effect::Restart => APPLIED_COLD,
        }
    }
}

/// D1: the reply says the old tab goes away, so a page the user had open in
/// it is not a surprise.
const APPLIED_HOT: &str = "The agent's next turn uses this; no restart needed. \
     Its current browser tab will be closed and a new one opened.";

const APPLIED_COLD: &str = "Restart the agent to apply this.";

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::agent::{McpServerEntry, McpServerNetwork};

    fn profile(entry: Option<McpServerEntry>) -> mur_common::AgentProfile {
        let mut p: mur_common::AgentProfile = serde_yaml::from_str(include_str!(
            "../../../../../mur-common/tests/fixtures/minimal_profile.yaml"
        ))
        .unwrap();
        p.mcp_servers = entry.into_iter().collect();
        p
    }

    fn browser(mode: McpNetMode, hosts: &[&str]) -> McpServerEntry {
        McpServerEntry {
            name: "browser".into(),
            command: "/opt/mur/bin/mur".into(),
            binary_sha256: Some("ab".into()),
            network: Some(McpServerNetwork {
                mode,
                allow_hosts: hosts.iter().map(|h| (*h).to_owned()).collect(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn a_host_edit_on_the_existing_entry_is_next_turn() {
        let before = profile(Some(browser(McpNetMode::Restricted, &["a.com"])));
        let after = profile(Some(browser(McpNetMode::Restricted, &["b.com"])));
        assert_eq!(Effect::of_write(&before, &after), Effect::NextTurn);
    }

    #[test]
    fn creating_the_entry_switching_mode_or_granting_needs_a_restart() {
        let none = profile(None);
        let restricted = profile(Some(browser(McpNetMode::Restricted, &["a.com"])));
        let research = profile(Some(browser(McpNetMode::BroadAudited, &[])));
        assert_eq!(Effect::of_write(&none, &restricted), Effect::Restart);
        assert_eq!(Effect::of_write(&restricted, &research), Effect::Restart);
        let mut granted = restricted.clone();
        granted
            .entitlements
            .filesystem
            .write
            .push("/x/browser".into());
        assert_eq!(Effect::of_write(&restricted, &granted), Effect::Restart);
    }

    #[test]
    fn the_preview_guesses_hot_only_for_a_same_mode_edit_without_repairs() {
        use McpNetMode::*;
        assert_eq!(
            Effect::predicted(Some(Restricted), Restricted, false),
            Effect::NextTurn
        );
        assert_eq!(
            Effect::predicted(Some(Restricted), Restricted, true),
            Effect::Restart
        );
        assert_eq!(
            Effect::predicted(Some(Restricted), BroadAudited, false),
            Effect::Restart
        );
        assert_eq!(Effect::predicted(None, Restricted, false), Effect::Restart);
    }

    #[test]
    fn the_hot_reply_says_the_tab_closes_and_no_restart() {
        let line = Effect::NextTurn.applied_line();
        assert!(line.contains("next turn") && line.contains("tab will be closed"));
        assert!(Effect::Restart.applied_line().contains("Restart"));
    }
}
