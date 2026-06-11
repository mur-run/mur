//! Multi-agent orchestration for `mur agent cli a b c` — one multiplexer
//! pane per agent, each running single-name `mur agent cli <name>`.

use anyhow::{Result, bail};

/// Which orchestration backend will host the panes.
#[allow(dead_code)] // used by Task 5's executor
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Already inside tmux → new window in the current session.
    TmuxInside,
    /// Already inside zellij → new tab + `zellij run` per agent.
    ZellijInside,
    /// Inside WezTerm → `wezterm cli split-pane` per agent.
    WezTerm,
    /// Inside kitty → `kitten @ launch`; may fail if remote control is off.
    Kitty,
    /// Not inside a multiplexer, tmux on PATH → new detached session + attach.
    TmuxNew,
    /// Not inside a multiplexer, zellij on PATH → `--layout-string`.
    ZellijNew,
}

/// Pure detection: first match wins, per the spec's table. `env` and
/// `on_path` are injected so tests need no real environment.
#[allow(dead_code)] // used by Task 5's executor
pub fn detect(
    env: impl Fn(&str) -> Option<String>,
    on_path: impl Fn(&str) -> bool,
) -> Option<Backend> {
    if env("TMUX").is_some() {
        return Some(Backend::TmuxInside);
    }
    if env("ZELLIJ").is_some() {
        return Some(Backend::ZellijInside);
    }
    if env("WEZTERM_PANE").is_some() {
        return Some(Backend::WezTerm);
    }
    if env("KITTY_WINDOW_ID").is_some() {
        return Some(Backend::Kitty);
    }
    if on_path("tmux") {
        return Some(Backend::TmuxNew);
    }
    if on_path("zellij") {
        return Some(Backend::ZellijNew);
    }
    None
}

pub fn run(names: &[String], _resume: bool, _auto: bool) -> Result<()> {
    bail!("multi-agent mode not yet implemented: {}", names.join(", "));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of<'a>(set: &'a [&'a str]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| set.contains(&k).then(|| "1".to_string())
    }

    #[test]
    fn detect_prefers_inside_multiplexer_over_path() {
        let d = detect(env_of(&["TMUX"]), |_| true);
        assert_eq!(d, Some(Backend::TmuxInside));
        let d = detect(env_of(&["ZELLIJ"]), |_| true);
        assert_eq!(d, Some(Backend::ZellijInside));
        let d = detect(env_of(&["WEZTERM_PANE"]), |_| true);
        assert_eq!(d, Some(Backend::WezTerm));
        let d = detect(env_of(&["KITTY_WINDOW_ID"]), |_| true);
        assert_eq!(d, Some(Backend::Kitty));
    }

    #[test]
    fn detect_tmux_wins_over_other_env() {
        // $TMUX beats $WEZTERM_PANE (tmux running inside WezTerm).
        let d = detect(env_of(&["TMUX", "WEZTERM_PANE"]), |_| false);
        assert_eq!(d, Some(Backend::TmuxInside));
    }

    #[test]
    fn detect_falls_back_to_path_then_none() {
        let d = detect(env_of(&[]), |p| p == "tmux");
        assert_eq!(d, Some(Backend::TmuxNew));
        let d = detect(env_of(&[]), |p| p == "zellij");
        assert_eq!(d, Some(Backend::ZellijNew));
        let d = detect(env_of(&[]), |_| false);
        assert_eq!(d, None);
    }
}
