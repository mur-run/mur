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

/// argv for one pane: single-name `mur agent cli` with forwarded flags.
#[allow(dead_code)] // used by Task 5's executor
fn pane_argv(exe: &str, name: &str, resume: bool, auto: bool) -> Vec<String> {
    let mut v = vec![exe.to_string(), "agent".into(), "cli".into(), name.into()];
    if resume {
        v.push("--resume".into());
    }
    if auto {
        v.push("--auto".into());
    }
    v
}

/// POSIX single-quote escaping for tmux's shell_command argument (the exe
/// path can contain spaces, e.g. /Volumes/My Drive/...).
#[allow(dead_code)] // used by Task 5's executor
fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// One pane's argv joined into a tmux shell_command string.
#[allow(dead_code)] // used by Task 5's executor
fn pane_shell(exe: &str, name: &str, resume: bool, auto: bool) -> String {
    pane_argv(exe, name, resume, auto)
        .iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Outside tmux: detached session, one pane per agent, tiled, then attach.
#[allow(dead_code)] // used by Task 5's executor
fn tmux_new_session(
    session: &str,
    exe: &str,
    names: &[String],
    resume: bool,
    auto: bool,
) -> Vec<Vec<String>> {
    let target = format!("={session}");
    let mut cmds = vec![vec![
        "tmux".into(),
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        session.into(),
        pane_shell(exe, &names[0], resume, auto),
    ]];
    for name in &names[1..] {
        cmds.push(vec![
            "tmux".into(),
            "split-window".into(),
            "-t".into(),
            target.clone(),
            pane_shell(exe, name, resume, auto),
        ]);
        cmds.push(vec![
            "tmux".into(),
            "select-layout".into(),
            "-t".into(),
            target.clone(),
            "tiled".into(),
        ]);
    }
    cmds.push(vec![
        "tmux".into(),
        "attach-session".into(),
        "-t".into(),
        target,
    ]);
    cmds
}

/// Inside tmux: open a new window (printing its id for targeting) running
/// the first agent. The user's current window is untouched.
#[allow(dead_code)] // used by Task 5's executor
fn tmux_inside_open(exe: &str, names: &[String], resume: bool, auto: bool) -> Vec<String> {
    vec![
        "tmux".into(),
        "new-window".into(),
        "-P".into(),
        "-F".into(),
        "#{window_id}".into(),
        pane_shell(exe, &names[0], resume, auto),
    ]
}

/// Inside tmux: split the captured window id for each remaining agent,
/// retiling after every split.
#[allow(dead_code)] // used by Task 5's executor
fn tmux_inside_rest(
    window_id: &str,
    exe: &str,
    names: &[String],
    resume: bool,
    auto: bool,
) -> Vec<Vec<String>> {
    let mut cmds = Vec::new();
    for name in &names[1..] {
        cmds.push(vec![
            "tmux".into(),
            "split-window".into(),
            "-t".into(),
            window_id.into(),
            pane_shell(exe, name, resume, auto),
        ]);
        cmds.push(vec![
            "tmux".into(),
            "select-layout".into(),
            "-t".into(),
            window_id.into(),
            "tiled".into(),
        ]);
    }
    cmds
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
        // $ZELLIJ beats $WEZTERM_PANE (zellij running inside WezTerm).
        let d = detect(env_of(&["ZELLIJ", "WEZTERM_PANE"]), |_| false);
        assert_eq!(d, Some(Backend::ZellijInside));
        // Contract is "present ⇒ inside", not "non-empty ⇒ inside".
        let d = detect(|_| Some(String::new()), |_| false);
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

    #[test]
    fn pane_argv_includes_flags() {
        let v = pane_argv("/opt/homebrew/bin/mur", "a1", true, true);
        assert_eq!(
            v,
            vec![
                "/opt/homebrew/bin/mur",
                "agent",
                "cli",
                "a1",
                "--resume",
                "--auto"
            ]
        );
        let v = pane_argv("/opt/homebrew/bin/mur", "a1", false, false);
        assert_eq!(v, vec!["/opt/homebrew/bin/mur", "agent", "cli", "a1"]);
    }

    #[test]
    fn shell_quote_handles_spaces_and_single_quotes() {
        assert_eq!(shell_quote("plain"), "plain");
        assert_eq!(
            shell_quote("/Volumes/My Drive/mur"),
            "'/Volumes/My Drive/mur'"
        );
        assert_eq!(shell_quote("it's"), r#"'it'\''s'"#);
    }

    #[test]
    fn tmux_new_session_plan_shape() {
        let names = vec!["a1".to_string(), "a2".to_string(), "a3".to_string()];
        let cmds = tmux_new_session("mur-chat", "/bin/mur", &names, false, false);
        assert_eq!(
            cmds[0],
            vec![
                "tmux",
                "new-session",
                "-d",
                "-s",
                "mur-chat",
                "/bin/mur agent cli a1"
            ]
        );
        // Each later agent: split + retile (retile after each split avoids
        // "pane too small" when opening many panes).
        assert_eq!(
            cmds[1],
            vec![
                "tmux",
                "split-window",
                "-t",
                "=mur-chat",
                "/bin/mur agent cli a2"
            ]
        );
        assert_eq!(
            cmds[2],
            vec!["tmux", "select-layout", "-t", "=mur-chat", "tiled"]
        );
        assert_eq!(
            cmds[3],
            vec![
                "tmux",
                "split-window",
                "-t",
                "=mur-chat",
                "/bin/mur agent cli a3"
            ]
        );
        assert_eq!(
            cmds[4],
            vec!["tmux", "select-layout", "-t", "=mur-chat", "tiled"]
        );
        assert_eq!(cmds[5], vec!["tmux", "attach-session", "-t", "=mur-chat"]);
        assert_eq!(cmds.len(), 6);
    }

    #[test]
    fn tmux_inside_plan_shape() {
        let names = vec!["a1".to_string(), "a2".to_string()];
        let open = tmux_inside_open("/bin/mur", &names, false, false);
        assert_eq!(
            open,
            vec![
                "tmux",
                "new-window",
                "-P",
                "-F",
                "#{window_id}",
                "/bin/mur agent cli a1"
            ]
        );
        let rest = tmux_inside_rest("@7", "/bin/mur", &names, false, false);
        assert_eq!(
            rest[0],
            vec!["tmux", "split-window", "-t", "@7", "/bin/mur agent cli a2"]
        );
        assert_eq!(rest[1], vec!["tmux", "select-layout", "-t", "@7", "tiled"]);
        assert_eq!(rest.len(), 2);
    }
}
