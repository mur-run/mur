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

/// Per remaining agent (`names[1..]`): a `split-window` + `select-layout
/// tiled` command pair. Retiling after every split avoids tmux's
/// "pane too small" refusal when opening many panes.
#[allow(dead_code)] // used by Task 5's executor
fn split_and_tile(
    target: &str,
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
            target.into(),
            pane_shell(exe, name, resume, auto),
        ]);
        cmds.push(vec![
            "tmux".into(),
            "select-layout".into(),
            "-t".into(),
            target.into(),
            "tiled".into(),
        ]);
    }
    cmds
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
    debug_assert!(
        !names.is_empty(),
        "pane planning requires at least one name"
    );
    // Splits target pane 0 explicitly; a bare session target would resolve
    // to whichever pane tmux considers active. attach-session takes a
    // session target, not a pane target.
    let pane_target = format!("={session}:.0");
    let mut cmds = vec![vec![
        "tmux".into(),
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        session.into(),
        pane_shell(exe, &names[0], resume, auto),
    ]];
    cmds.extend(split_and_tile(&pane_target, exe, names, resume, auto));
    cmds.push(vec![
        "tmux".into(),
        "attach-session".into(),
        "-t".into(),
        format!("={session}"),
    ]);
    cmds
}

/// Inside tmux: open a new window (printing its id for targeting) running
/// the first agent. Focus moves to the new window by design; the user's
/// previous window keeps its panes and content.
#[allow(dead_code)] // used by Task 5's executor
fn tmux_inside_open(exe: &str, names: &[String], resume: bool, auto: bool) -> Vec<String> {
    debug_assert!(
        !names.is_empty(),
        "pane planning requires at least one name"
    );
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
    split_and_tile(window_id, exe, names, resume, auto)
}

/// Display label for the spawned tab/window across backends.
#[allow(dead_code)] // used by Task 5's executor
const CHAT_LABEL: &str = "mur-chat";

/// Inside zellij: new named tab, then one `zellij run` pane per agent
/// (panes land in the freshly focused tab).
#[allow(dead_code)] // used by Task 5's executor
fn zellij_inside(exe: &str, names: &[String], resume: bool, auto: bool) -> Vec<Vec<String>> {
    let mut cmds = vec![vec![
        "zellij".into(),
        "action".into(),
        "new-tab".into(),
        "--name".into(),
        CHAT_LABEL.into(),
    ]];
    for name in names {
        let mut c = vec!["zellij".into(), "run".into(), "--".into()];
        c.extend(pane_argv(exe, name, resume, auto));
        cmds.push(c);
    }
    cmds
}

/// KDL string escaping (paths with `"` or `\` are unlikely but cheap to handle).
#[allow(dead_code)] // used by Task 5's executor
fn kdl_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', r"\\").replace('"', "\\\""))
}

/// Outside zellij: generated layout for `zellij --layout-string`.
#[allow(dead_code)] // used by Task 5's executor
fn zellij_kdl_layout(exe: &str, names: &[String], resume: bool, auto: bool) -> String {
    let mut out = String::from("layout {\n    pane split_direction=\"vertical\" {\n");
    for name in names {
        let args: Vec<String> = pane_argv(exe, name, resume, auto)[1..]
            .iter()
            .map(|a| kdl_quote(a))
            .collect();
        out.push_str(&format!(
            "        pane command={} {{ args {}; }}\n",
            kdl_quote(exe),
            args.join(" ")
        ));
    }
    out.push_str("    }\n}\n");
    out
}

/// Inside WezTerm: split the current pane once per agent, alternating
/// right/bottom for a rough grid.
#[allow(dead_code)] // used by Task 5's executor
fn wezterm_splits(exe: &str, names: &[String], resume: bool, auto: bool) -> Vec<Vec<String>> {
    names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let dir = if i % 2 == 0 { "--right" } else { "--bottom" };
            let mut c: Vec<String> = vec![
                "wezterm".into(),
                "cli".into(),
                "split-pane".into(),
                dir.into(),
                "--".into(),
            ];
            c.extend(pane_argv(exe, name, resume, auto));
            c
        })
        .collect()
}

/// Inside kitty: one `kitten @ launch` per agent, alternating split axis.
/// Requires `allow_remote_control` — failure falls back at execution time.
#[allow(dead_code)] // used by Task 5's executor
fn kitty_launches(exe: &str, names: &[String], resume: bool, auto: bool) -> Vec<Vec<String>> {
    names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let loc = if i % 2 == 0 {
                "--location=vsplit"
            } else {
                "--location=hsplit"
            };
            let mut c: Vec<String> = vec![
                "kitten".into(),
                "@".into(),
                "launch".into(),
                loc.into(),
                "--".into(),
            ];
            c.extend(pane_argv(exe, name, resume, auto));
            c
        })
        .collect()
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
                "=mur-chat:.0",
                "/bin/mur agent cli a2"
            ]
        );
        assert_eq!(
            cmds[2],
            vec!["tmux", "select-layout", "-t", "=mur-chat:.0", "tiled"]
        );
        assert_eq!(
            cmds[3],
            vec![
                "tmux",
                "split-window",
                "-t",
                "=mur-chat:.0",
                "/bin/mur agent cli a3"
            ]
        );
        assert_eq!(
            cmds[4],
            vec!["tmux", "select-layout", "-t", "=mur-chat:.0", "tiled"]
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

    #[test]
    fn zellij_inside_plan_shape() {
        let names = vec!["a1".to_string(), "a2".to_string()];
        let cmds = zellij_inside("/bin/mur", &names, false, true);
        assert_eq!(
            cmds[0],
            vec!["zellij", "action", "new-tab", "--name", "mur-chat"]
        );
        assert_eq!(
            cmds[1],
            vec![
                "zellij", "run", "--", "/bin/mur", "agent", "cli", "a1", "--auto"
            ]
        );
        assert_eq!(
            cmds[2],
            vec![
                "zellij", "run", "--", "/bin/mur", "agent", "cli", "a2", "--auto"
            ]
        );
        assert_eq!(cmds.len(), 3);
    }

    #[test]
    fn zellij_kdl_layout_quotes_and_lists_all_agents() {
        let names = vec!["a1".to_string(), "a2".to_string()];
        let kdl = zellij_kdl_layout("/My Drive/mur", &names, true, false);
        let expected = concat!(
            "layout {\n",
            "    pane split_direction=\"vertical\" {\n",
            "        pane command=\"/My Drive/mur\" { args \"agent\" \"cli\" \"a1\" \"--resume\"; }\n",
            "        pane command=\"/My Drive/mur\" { args \"agent\" \"cli\" \"a2\" \"--resume\"; }\n",
            "    }\n",
            "}\n",
        );
        assert_eq!(kdl, expected);
    }

    #[test]
    fn wezterm_plan_alternates_direction() {
        let names = vec!["a1".to_string(), "a2".to_string(), "a3".to_string()];
        let cmds = wezterm_splits("/bin/mur", &names, false, false);
        assert_eq!(
            cmds[0],
            vec![
                "wezterm",
                "cli",
                "split-pane",
                "--right",
                "--",
                "/bin/mur",
                "agent",
                "cli",
                "a1"
            ]
        );
        assert_eq!(
            cmds[1],
            vec![
                "wezterm",
                "cli",
                "split-pane",
                "--bottom",
                "--",
                "/bin/mur",
                "agent",
                "cli",
                "a2"
            ]
        );
        assert_eq!(
            cmds[2],
            vec![
                "wezterm",
                "cli",
                "split-pane",
                "--right",
                "--",
                "/bin/mur",
                "agent",
                "cli",
                "a3"
            ]
        );
    }

    #[test]
    fn kitty_plan_alternates_location() {
        let names = vec!["a1".to_string(), "a2".to_string()];
        let cmds = kitty_launches("/bin/mur", &names, false, false);
        assert_eq!(
            cmds[0],
            vec![
                "kitten",
                "@",
                "launch",
                "--location=vsplit",
                "--",
                "/bin/mur",
                "agent",
                "cli",
                "a1"
            ]
        );
        assert_eq!(
            cmds[1],
            vec![
                "kitten",
                "@",
                "launch",
                "--location=hsplit",
                "--",
                "/bin/mur",
                "agent",
                "cli",
                "a2"
            ]
        );
    }
}
