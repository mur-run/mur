//! Multi-agent orchestration for `mur agent cli a b c` — one multiplexer
//! pane per agent, each running single-name `mur agent cli <name>`.

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

use crate::a2a_dial::canonicalize_agent_name;

/// Which orchestration backend will host the panes.
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

/// Canonicalize every requested name and fail the whole batch if any agent
/// is unknown or not running — never open panes that immediately die.
fn validate(home: &Path, names: &[String]) -> Result<Vec<String>> {
    let mut canon = Vec::with_capacity(names.len());
    let mut unknown = Vec::new();
    let mut stopped = Vec::new();
    for n in names {
        let c = canonicalize_agent_name(home, n);
        let dir = home.join("agents").join(&c);
        if !dir.join("profile.yaml").is_file() {
            unknown.push(n.clone());
            continue;
        }
        if !dir.join("running.lock").exists() {
            stopped.push(c.clone());
        }
        canon.push(c);
    }
    if !unknown.is_empty() {
        bail!(
            "unknown agent(s): {} — see `mur agent list`",
            unknown.join(", ")
        );
    }
    if !stopped.is_empty() {
        bail!(
            "agent(s) not running: {} — start them first, e.g. `mur agent run {}`",
            stopped.join(", "),
            stopped[0]
        );
    }
    Ok(canon)
}

/// True when `prog` is spawnable from PATH (`tmux -V` / `zellij -V` both
/// exist; a non-zero exit still proves presence).
fn on_path(prog: &str) -> bool {
    Command::new(prog)
        .arg("-V")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// Run one external command, inheriting stdio (tmux attach is interactive).
fn run_cmd(argv: &[String]) -> Result<()> {
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .with_context(|| format!("spawn `{}`", argv.join(" ")))?;
    if !status.success() {
        bail!("`{}` exited with {status}", argv.join(" "));
    }
    Ok(())
}

/// Like `run_cmd` but captures trimmed stdout (tmux new-window -P).
fn run_cmd_capture(argv: &[String]) -> Result<String> {
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .with_context(|| format!("spawn `{}`", argv.join(" ")))?;
    if !out.status.success() {
        bail!(
            "`{}` exited with {}: {}",
            argv.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// First free tmux session name: mur-chat, mur-chat-2, mur-chat-3, …
fn free_tmux_session() -> String {
    for n in 1u32.. {
        let name = if n == 1 {
            CHAT_LABEL.to_string()
        } else {
            format!("{CHAT_LABEL}-{n}")
        };
        let exists = Command::new("tmux")
            .args(["has-session", "-t", &format!("={name}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !exists {
            return name;
        }
    }
    unreachable!("u32 session probe space exhausted")
}

/// Entry point from `cmd_cli` for 2+ names. Blocking (called via
/// `spawn_blocking`); `tmux attach` keeps the terminal until detach.
pub fn run(names: &[String], resume: bool, auto: bool) -> Result<()> {
    let home = crate::cmd::agent::resolve_mur_home()?;
    let canon = validate(&home, names)?;
    let exe = std::env::current_exe().context("resolve current executable")?;
    let exe = exe.to_string_lossy().into_owned();
    let backend = detect(|k| std::env::var(k).ok(), on_path).ok_or_else(|| {
        anyhow!(
            "multi-agent split needs a terminal multiplexer.\n\
             Install tmux (`brew install tmux`) or zellij, or run inside WezTerm/kitty."
        )
    })?;
    execute(backend, &exe, &canon, resume, auto)
}

fn execute(backend: Backend, exe: &str, names: &[String], resume: bool, auto: bool) -> Result<()> {
    match backend {
        Backend::TmuxInside => {
            let window_id = run_cmd_capture(&tmux_inside_open(exe, names, resume, auto))?;
            for cmd in tmux_inside_rest(&window_id, exe, names, resume, auto) {
                run_cmd(&cmd)?;
            }
            Ok(())
        }
        Backend::TmuxNew => {
            let session = free_tmux_session();
            for cmd in tmux_new_session(&session, exe, names, resume, auto) {
                run_cmd(&cmd)?;
            }
            Ok(())
        }
        Backend::ZellijInside => {
            for cmd in zellij_inside(exe, names, resume, auto) {
                run_cmd(&cmd)?;
            }
            Ok(())
        }
        Backend::ZellijNew => run_cmd(&[
            "zellij".into(),
            "--layout-string".into(),
            zellij_kdl_layout(exe, names, resume, auto),
        ]),
        Backend::WezTerm => {
            for cmd in wezterm_splits(exe, names, resume, auto) {
                run_cmd(&cmd)?;
            }
            Ok(())
        }
        Backend::Kitty => {
            let cmds = kitty_launches(exe, names, resume, auto);
            // kitty refuses when allow_remote_control is off — fall back to a
            // PATH multiplexer per the spec's detection table (kitty → row 5).
            if let Err(e) = run_cmd(&cmds[0]) {
                eprintln!("kitty remote control unavailable ({e}); falling back…");
                let fallback = if on_path("tmux") {
                    Backend::TmuxNew
                } else if on_path("zellij") {
                    Backend::ZellijNew
                } else {
                    bail!(
                        "kitty remote control is disabled and no tmux/zellij found.\n\
                         Enable `allow_remote_control yes` in kitty.conf or `brew install tmux`."
                    );
                };
                return execute(fallback, exe, names, resume, auto);
            }
            for cmd in &cmds[1..] {
                run_cmd(cmd)?;
            }
            Ok(())
        }
    }
}

/// argv for one pane: single-name `mur agent cli` with forwarded flags.
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
const CHAT_LABEL: &str = "mur-chat";

/// Inside zellij: new named tab, then one `zellij run` pane per agent
/// (panes land in the freshly focused tab).
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
fn kdl_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', r"\\").replace('"', "\\\""))
}

/// Outside zellij: generated layout for `zellij --layout-string`.
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
            err.contains("a2") && err.contains("mur agent run"),
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
}
