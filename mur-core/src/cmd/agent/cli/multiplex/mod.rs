//! Multi-agent orchestration for `mur agent cli a b c` — one multiplexer
//! pane per agent, each running single-name `mur agent cli <name>`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
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
        } else {
            canon.push(c);
        }
    }
    if !unknown.is_empty() {
        bail!(
            "unknown agent(s): {} — see `mur agent list`",
            unknown.join(", ")
        );
    }
    if !stopped.is_empty() {
        bail!(
            "agent(s) not running: {} — start them first, e.g. `mur agent install-service {}`",
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
    let exe = canonical_mur_exe(exe).to_string_lossy().into_owned();
    let backend = detect(|k| std::env::var(k).ok(), on_path).ok_or_else(|| {
        anyhow!(
            "multi-agent split needs a terminal multiplexer.\n\
             Install tmux (`brew install tmux`) or zellij, or run inside WezTerm/kitty."
        )
    })?;
    execute(backend, &exe, &canon, resume, auto)
}

/// Normalize the launching binary to the canonical `mur` for pane commands.
///
/// Every pane command is built as `<exe> agent cli <name>` (see `pane_argv`),
/// which is only correct when `<exe>` is the `mur` binary. We may instead be
/// launched via the `murmur` alias — a symlink to the same binary, BusyBox
/// convention — in which case `current_exe()` reports the `murmur` path. But
/// `murmur <args>` already expands to `mur agent cli <args>` (see
/// `crate::cli::murmur`), so appending `agent cli` would double-dispatch into
/// `mur agent cli agent cli <name>`, failing with `unknown agent(s): agent,
/// cli`. That kills the pane process immediately, tmux reaps the now-empty
/// session, and the trailing `attach-session` reports "no sessions" / exits 1.
///
/// Rewrite a `murmur` invocation to its sibling `mur`, preserving directory
/// and extension. The stem check mirrors `cli::murmur::is_murmur_invocation`,
/// duplicated here because that module lives in the binary crate only and this
/// pane-planning code compiles as part of the library crate.
///
/// Shared with sibling slash commands that respawn the binary with a
/// top-level subcommand (e.g. `/deep-research` → `<exe> deep-research …`),
/// which hit the same double-dispatch under the `murmur` alias.
pub(super) fn canonical_mur_exe(exe: PathBuf) -> PathBuf {
    let is_murmur = exe
        .file_stem()
        .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case("murmur"));
    if !is_murmur {
        return exe;
    }
    let new_name = match exe.extension() {
        Some(ext) => {
            let mut s = OsString::from("mur.");
            s.push(ext);
            s
        }
        None => OsString::from("mur"),
    };
    exe.with_file_name(new_name)
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
    // Auto-approve is the pane's default too; only the opt-out has to travel.
    if !auto {
        v.push("--ask".into());
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

/// Persistent status-bar hint shown in sessions we create, so a user who
/// doesn't know tmux can discover how to move between agent panes. The
/// default prefix is Ctrl-b (we set no `~/.tmux.conf` override on the
/// session), so the keys named here are the ones actually bound.
const MUR_TMUX_HINT: &str = " MUR · click a pane to type · Ctrl-b ←/→ switch · Ctrl-b d detach ";

/// Make a session we created navigable without prior tmux knowledge:
/// `mouse on` gives click-to-focus, and the status bar carries the hint.
/// Scoped to the named session (`-t`) so the user's global tmux config and
/// any other sessions are untouched.
///
/// `session_target` must be the trailing-colon form (`=name:`): unlike
/// session-target commands (`attach-session`, `has-session`), `set-option`
/// rejects a bare `=name` ("no such session") and only resolves the exact
/// name through the window-target parser that the `:` selects.
fn tmux_session_setup(session_target: &str) -> Vec<Vec<String>> {
    let opt = |key: &str, val: &str| {
        vec![
            "tmux".into(),
            "set-option".into(),
            "-t".into(),
            session_target.to_string(),
            key.into(),
            val.into(),
        ]
    };
    vec![
        opt("mouse", "on"),
        opt("status-right-length", "120"),
        opt("status-right", MUR_TMUX_HINT),
    ]
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
    let session_target = format!("={session}");
    let pane_target = format!("={session}:.0");
    let mut cmds = vec![vec![
        "tmux".into(),
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        session.into(),
        pane_shell(exe, &names[0], resume, auto),
    ]];
    // Click-to-focus + navigation hint before the panes exist, so the
    // session is usable the moment `attach-session` hands over the terminal.
    // `set-option` needs the `=name:` window-target form (see fn docs).
    cmds.extend(tmux_session_setup(&format!("={session}:")));
    cmds.extend(split_and_tile(&pane_target, exe, names, resume, auto));
    cmds.push(vec![
        "tmux".into(),
        "attach-session".into(),
        "-t".into(),
        session_target,
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
    debug_assert!(
        !names.is_empty(),
        "pane planning requires at least one name"
    );
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
    debug_assert!(
        !names.is_empty(),
        "pane planning requires at least one name"
    );
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
    debug_assert!(
        !names.is_empty(),
        "pane planning requires at least one name"
    );
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
    debug_assert!(
        !names.is_empty(),
        "pane planning requires at least one name"
    );
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
mod tests;
