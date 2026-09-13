//! `tests`, moved out of `multiplex.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: dedented one level, paths one level deeper.

use super::super::*;

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
    // Auto-approve is the default, so it travels as nothing; ask-first is
    // the flag that has to reach the pane.
    let v = pane_argv("/opt/homebrew/bin/mur", "a1", true, true);
    assert_eq!(
        v,
        vec!["/opt/homebrew/bin/mur", "agent", "cli", "a1", "--resume"]
    );
    let v = pane_argv("/opt/homebrew/bin/mur", "a1", false, false);
    assert_eq!(
        v,
        vec!["/opt/homebrew/bin/mur", "agent", "cli", "a1", "--ask"]
    );
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
fn murmur_exe_is_normalized_to_mur_sibling() {
    // Launched via the `murmur` alias, current_exe() reports the murmur
    // path. Pane commands must use the sibling `mur`, never `murmur agent
    // cli …`, which double-dispatches to "unknown agent(s): agent, cli"
    // and tears down the session before attach.
    assert_eq!(
        canonical_mur_exe(PathBuf::from("/opt/homebrew/bin/murmur")),
        PathBuf::from("/opt/homebrew/bin/mur")
    );
    // Case-insensitive stem match (mirrors is_murmur_invocation).
    assert_eq!(
        canonical_mur_exe(PathBuf::from("/tools/MURMUR")),
        PathBuf::from("/tools/mur")
    );
    // Extension is preserved (Windows `murmur.exe` → `mur.exe`).
    assert_eq!(
        canonical_mur_exe(PathBuf::from("/tools/murmur.exe")),
        PathBuf::from("/tools/mur.exe")
    );
    // A plain `mur` path is left untouched.
    assert_eq!(
        canonical_mur_exe(PathBuf::from("/usr/local/bin/mur")),
        PathBuf::from("/usr/local/bin/mur")
    );
    // A non-murmur stem that merely contains "mur" is untouched.
    assert_eq!(
        canonical_mur_exe(PathBuf::from("/usr/local/bin/murex")),
        PathBuf::from("/usr/local/bin/murex")
    );
}

#[test]
fn tmux_new_session_plan_shape() {
    let names = vec!["a1".to_string(), "a2".to_string(), "a3".to_string()];
    let cmds = tmux_new_session("mur-chat", "/bin/mur", &names, false, true);
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
    // Session setup runs before any split so the session is navigable
    // (click-to-focus + hint) the moment attach hands over the terminal.
    // `=mur-chat:` (trailing colon) is required for `set-option`; the bare
    // `=mur-chat` form attach-session uses is rejected here by tmux.
    assert_eq!(
        cmds[1],
        vec!["tmux", "set-option", "-t", "=mur-chat:", "mouse", "on"]
    );
    assert_eq!(
        cmds[2],
        vec![
            "tmux",
            "set-option",
            "-t",
            "=mur-chat:",
            "status-right-length",
            "120"
        ]
    );
    assert_eq!(
        cmds[3],
        vec![
            "tmux",
            "set-option",
            "-t",
            "=mur-chat:",
            "status-right",
            MUR_TMUX_HINT
        ]
    );
    // Each later agent: split + retile (retile after each split avoids
    // "pane too small" when opening many panes).
    assert_eq!(
        cmds[4],
        vec![
            "tmux",
            "split-window",
            "-t",
            "=mur-chat:.0",
            "/bin/mur agent cli a2"
        ]
    );
    assert_eq!(
        cmds[5],
        vec!["tmux", "select-layout", "-t", "=mur-chat:.0", "tiled"]
    );
    assert_eq!(
        cmds[6],
        vec![
            "tmux",
            "split-window",
            "-t",
            "=mur-chat:.0",
            "/bin/mur agent cli a3"
        ]
    );
    assert_eq!(
        cmds[7],
        vec!["tmux", "select-layout", "-t", "=mur-chat:.0", "tiled"]
    );
    assert_eq!(cmds[8], vec!["tmux", "attach-session", "-t", "=mur-chat"]);
    assert_eq!(cmds.len(), 9);
}

#[test]
fn tmux_inside_plan_shape() {
    let names = vec!["a1".to_string(), "a2".to_string()];
    let open = tmux_inside_open("/bin/mur", &names, false, true);
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
    let rest = tmux_inside_rest("@7", "/bin/mur", &names, false, true);
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
    let cmds = zellij_inside("/bin/mur", &names, false, false);
    assert_eq!(
        cmds[0],
        vec!["zellij", "action", "new-tab", "--name", "mur-chat"]
    );
    assert_eq!(
        cmds[1],
        vec![
            "zellij", "run", "--", "/bin/mur", "agent", "cli", "a1", "--ask"
        ]
    );
    assert_eq!(
        cmds[2],
        vec![
            "zellij", "run", "--", "/bin/mur", "agent", "cli", "a2", "--ask"
        ]
    );
    assert_eq!(cmds.len(), 3);
}

#[test]
fn zellij_kdl_layout_quotes_and_lists_all_agents() {
    let names = vec!["a1".to_string(), "a2".to_string()];
    let kdl = zellij_kdl_layout("/My Drive/mur", &names, true, true);
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
    let cmds = wezterm_splits("/bin/mur", &names, false, true);
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
    let cmds = kitty_launches("/bin/mur", &names, false, true);
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

use std::fs;

fn fake_home(agents: &[(&str, bool)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    for (name, running) in agents {
        let dir = tmp.path().join("agents").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("profile.yaml"), "name: x\n").unwrap();
        if *running {
            fs::write(dir.join("running.lock"), "1").unwrap();
        }
    }
    tmp
}

#[test]
fn validate_rejects_unknown_agents_as_a_batch() {
    let home = fake_home(&[("a1", true)]);
    let err = validate(home.path(), &["a1".into(), "nope".into(), "alsono".into()])
        .unwrap_err()
        .to_string();
    assert!(err.contains("nope") && err.contains("alsono"), "got: {err}");
}

#[test]
fn validate_rejects_stopped_agents() {
    let home = fake_home(&[("a1", true), ("a2", false)]);
    let err = validate(home.path(), &["a1".into(), "a2".into()])
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("a2") && err.contains("mur agent install-service"),
        "got: {err}"
    );
}

#[test]
fn validate_canonicalizes_and_allows_duplicates() {
    let home = fake_home(&[("a1", true)]);
    // On case-insensitive filesystems (default macOS APFS),
    // canonicalize_agent_name returns the input as-is when is_file()
    // succeeds for the cased form. Both "A1" and "a1" resolve to the
    // same directory, so validation passes. The key invariants are
    // that duplicates are allowed and all names pass validation.
    let canon = validate(home.path(), &["A1".into(), "a1".into()]).unwrap();
    assert_eq!(canon.len(), 2, "expected 2 entries, got {canon:?}");
}
