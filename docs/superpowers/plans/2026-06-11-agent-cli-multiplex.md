# Agent CLI Multiplex + `murmur` Quick Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `mur agent cli a1 a2 a3` opens one multiplexer pane per agent (tmux primary, zellij/WezTerm/kitty auto-detected), and a `murmur` symlink makes `murmur <name…>` the quick form.

**Architecture:** Multi-name orchestration is a thin layer that spawns external multiplexer panes, each running the unchanged single-name `mur agent cli <name>`. Backend detection and command planning are pure functions (testable without a TTY); only a small executor touches `std::process::Command`. `murmur` is an argv[0]-dispatched symlink to `mur` (same BusyBox convention as `mur_agent_<name>`).

**Tech Stack:** Rust (edition 2024), clap derive, std::process. No new crate dependencies.

**Spec:** `docs/superpowers/specs/2026-06-11-agent-cli-multiplex-design.md`

**Branch:** work on a feature branch off `main` (e.g. `feat/agent-cli-multiplex`), created via superpowers:using-git-worktrees at execution time.

**Test command note:** CI uses nextest; plain `cargo test --workspace` has 7 known-flaky mur-core tests. Always run the *targeted* test commands given in each step (they only run the new tests), e.g. `cargo test -p mur-core multiplex`.

---

## File Structure

| File | Responsibility |
|---|---|
| `mur-core/src/cli/agent.rs` (modify) | clap: `Cli` variant takes `names: Vec<String>` (1..) |
| `mur-core/src/dispatch.rs` (modify, ~line 1047) | pass `names` slice through |
| `mur-core/src/cmd/agent/cli/mod.rs` (modify) | `cmd_cli(&[String], …)`; >1 name → `multiplex::run` |
| `mur-core/src/cmd/agent/cli/multiplex.rs` (create) | detection, pane argv planning, KDL layout, executor |
| `mur-core/src/cli/murmur.rs` (create) | pure argv[0]/argv mapping for the `murmur` symlink |
| `mur-core/src/cli/mod.rs` (modify) | `pub mod murmur;` |
| `mur-core/src/main.rs` (modify) | argv[0] dispatch before `Cli::parse()` |
| `build.sh` (modify) | install `murmur` symlink next to `mur` |
| `.github/workflows/release.yml` (modify) | Homebrew formula `bin.install_symlink` |
| `README.md`, `CLAUDE.md`, `docs/architecture/runtime-overview.md` (modify) | docs |

Module-declaration gotcha: `cmd` is declared in BOTH `mur-core/src/lib.rs` and `mur-core/src/main.rs`, so `multiplex.rs` compiles in both crates — use `crate::cmd::agent::resolve_mur_home` style paths (valid in both). The `cli` clap module is declared only in `main.rs`, so `murmur.rs` tests run as bin tests (`cargo test -p mur-core --bin mur`).

---

### Task 1: clap surface — `names: Vec<String>`

**Files:**
- Modify: `mur-core/src/cli/agent.rs:74-84`
- Modify: `mur-core/src/dispatch.rs:1047`
- Modify: `mur-core/src/cmd/agent/cli/mod.rs:48-69` (`cmd_cli`)

- [ ] **Step 1: Write the failing tests** — append to the bottom of `mur-core/src/cli/agent.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::AgentAction;
    use crate::cli::{Cli, Commands};
    use clap::Parser;

    fn parse_cli_action(argv: &[&str]) -> AgentAction {
        let cli = Cli::try_parse_from(argv).expect("parse argv");
        match cli.command {
            Commands::Agent { action } => action,
            _ => panic!("expected Agent variant"),
        }
    }

    #[test]
    fn agent_cli_accepts_multiple_names() {
        let AgentAction::Cli { names, resume, auto } =
            parse_cli_action(&["mur", "agent", "cli", "a1", "a2", "a3", "--auto"])
        else {
            panic!("expected Cli variant");
        };
        assert_eq!(names, vec!["a1", "a2", "a3"]);
        assert!(!resume);
        assert!(auto);
    }

    #[test]
    fn agent_cli_single_name_still_parses() {
        let AgentAction::Cli { names, .. } = parse_cli_action(&["mur", "agent", "cli", "mur"])
        else {
            panic!("expected Cli variant");
        };
        assert_eq!(names, vec!["mur"]);
    }

    #[test]
    fn agent_cli_requires_at_least_one_name() {
        assert!(Cli::try_parse_from(["mur", "agent", "cli"]).is_err());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-core --bin mur agent_cli_accepts`
Expected: COMPILE ERROR (`names` does not exist; variant has `name: String`)

- [ ] **Step 3: Change the clap variant** in `mur-core/src/cli/agent.rs:75-84`:

```rust
    /// Interactive streaming TUI chat with an agent (the agent must be running)
    Cli {
        /// Agent name(s) — more than one opens each chat in its own split pane
        #[arg(required = true, num_args = 1..)]
        names: Vec<String>,
        /// Resume the most recent saved conversation for this agent
        #[arg(long)]
        resume: bool,
        /// Auto-approve every tool call for this session (no HITL prompts)
        #[arg(long)]
        auto: bool,
    },
```

- [ ] **Step 4: Update the dispatch site** in `mur-core/src/dispatch.rs:1047`:

```rust
        AgentAction::Cli { names, resume, auto } => cmd::agent::cmd_cli(&names, resume, auto).await?,
```

- [ ] **Step 5: Update `cmd_cli`** in `mur-core/src/cmd/agent/cli/mod.rs`. Replace the signature and the first lines of the existing function (the body from `let home = …` down stays identical, operating on `name`):

```rust
/// Entry point dispatched from `AgentAction::Cli`.
pub async fn cmd_cli(names: &[String], resume: bool, auto: bool) -> Result<()> {
    if names.len() > 1 {
        let names = names.to_vec();
        return tokio::task::spawn_blocking(move || multiplex::run(&names, resume, auto)).await?;
    }
    let name = &names[0];
    let home = super::resolve_mur_home()?;
    let agent = canonicalize_agent_name(&home, name);
    // … rest of the existing body unchanged …
}
```

Add the module declaration next to the other `mod` lines at the top of the same file:

```rust
mod multiplex;
```

And create a placeholder `mur-core/src/cmd/agent/cli/multiplex.rs` so it compiles (Task 2 fills it in):

```rust
//! Multi-agent orchestration for `mur agent cli a b c` — one multiplexer
//! pane per agent, each running single-name `mur agent cli <name>`.

use anyhow::{Result, bail};

pub fn run(names: &[String], _resume: bool, _auto: bool) -> Result<()> {
    bail!("multi-agent mode not yet implemented: {}", names.join(", "));
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test -p mur-core --bin mur agent_cli`
Expected: 3 passed

- [ ] **Step 7: Lint and commit**

```bash
cargo clippy -p mur-core -- -D warnings && cargo fmt
git add mur-core/src/cli/agent.rs mur-core/src/dispatch.rs mur-core/src/cmd/agent/cli/mod.rs mur-core/src/cmd/agent/cli/multiplex.rs
git commit -m "feat(agent-cli): accept multiple agent names in clap surface"
```

---

### Task 2: Backend detection (pure)

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/multiplex.rs`

- [ ] **Step 1: Write the failing tests** — append to `multiplex.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(set: &[&str]) -> impl Fn(&str) -> Option<String> + '_ {
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-core multiplex`
Expected: COMPILE ERROR (`Backend`, `detect` not defined)

- [ ] **Step 3: Implement** — add above the tests in `multiplex.rs`:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core multiplex`
Expected: 3 passed (each test asserts multiple cases)

- [ ] **Step 5: Commit**

```bash
cargo clippy -p mur-core -- -D warnings && cargo fmt
git add mur-core/src/cmd/agent/cli/multiplex.rs
git commit -m "feat(agent-cli): multiplexer backend detection"
```

---

### Task 3: Pane argv, shell quoting, tmux command planning (pure)

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/multiplex.rs`

- [ ] **Step 1: Write the failing tests** — add inside the existing `mod tests`:

```rust
    #[test]
    fn pane_argv_includes_flags() {
        let v = pane_argv("/opt/homebrew/bin/mur", "a1", true, true);
        assert_eq!(v, vec!["/opt/homebrew/bin/mur", "agent", "cli", "a1", "--resume", "--auto"]);
        let v = pane_argv("/opt/homebrew/bin/mur", "a1", false, false);
        assert_eq!(v, vec!["/opt/homebrew/bin/mur", "agent", "cli", "a1"]);
    }

    #[test]
    fn shell_quote_handles_spaces_and_single_quotes() {
        assert_eq!(shell_quote("plain"), "plain");
        assert_eq!(shell_quote("/Volumes/My Drive/mur"), "'/Volumes/My Drive/mur'");
        assert_eq!(shell_quote("it's"), r#"'it'\''s'"#);
    }

    #[test]
    fn tmux_new_session_plan_shape() {
        let names = vec!["a1".to_string(), "a2".to_string(), "a3".to_string()];
        let cmds = tmux_new_session("mur-chat", "/bin/mur", &names, false, false);
        assert_eq!(
            cmds[0],
            vec!["tmux", "new-session", "-d", "-s", "mur-chat", "/bin/mur agent cli a1"]
        );
        // Each later agent: split + retile (retile after each split avoids
        // "pane too small" when opening many panes).
        assert_eq!(
            cmds[1],
            vec!["tmux", "split-window", "-t", "=mur-chat", "/bin/mur agent cli a2"]
        );
        assert_eq!(cmds[2], vec!["tmux", "select-layout", "-t", "=mur-chat", "tiled"]);
        assert_eq!(
            cmds[3],
            vec!["tmux", "split-window", "-t", "=mur-chat", "/bin/mur agent cli a3"]
        );
        assert_eq!(cmds[4], vec!["tmux", "select-layout", "-t", "=mur-chat", "tiled"]);
        assert_eq!(cmds[5], vec!["tmux", "attach-session", "-t", "=mur-chat"]);
        assert_eq!(cmds.len(), 6);
    }

    #[test]
    fn tmux_inside_plan_shape() {
        let names = vec!["a1".to_string(), "a2".to_string()];
        let open = tmux_inside_open("/bin/mur", &names, false, false);
        assert_eq!(
            open,
            vec!["tmux", "new-window", "-P", "-F", "#{window_id}", "/bin/mur agent cli a1"]
        );
        let rest = tmux_inside_rest("@7", "/bin/mur", &names, false, false);
        assert_eq!(rest[0], vec!["tmux", "split-window", "-t", "@7", "/bin/mur agent cli a2"]);
        assert_eq!(rest[1], vec!["tmux", "select-layout", "-t", "@7", "tiled"]);
        assert_eq!(rest.len(), 2);
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-core multiplex`
Expected: COMPILE ERROR (functions not defined)

- [ ] **Step 3: Implement** the four pure functions:

```rust
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
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:".contains(c)) {
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

/// Outside tmux: detached session, one pane per agent, tiled, then attach.
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core multiplex`
Expected: 7 passed

- [ ] **Step 5: Commit**

```bash
cargo clippy -p mur-core -- -D warnings && cargo fmt
git add mur-core/src/cmd/agent/cli/multiplex.rs
git commit -m "feat(agent-cli): tmux pane planning + shell quoting"
```

---

### Task 4: zellij / WezTerm / kitty planning (pure)

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/multiplex.rs`

- [ ] **Step 1: Write the failing tests** — add inside `mod tests`:

```rust
    #[test]
    fn zellij_inside_plan_shape() {
        let names = vec!["a1".to_string(), "a2".to_string()];
        let cmds = zellij_inside("/bin/mur", &names, false, true);
        assert_eq!(cmds[0], vec!["zellij", "action", "new-tab", "--name", "mur-chat"]);
        assert_eq!(
            cmds[1],
            vec!["zellij", "run", "--", "/bin/mur", "agent", "cli", "a1", "--auto"]
        );
        assert_eq!(
            cmds[2],
            vec!["zellij", "run", "--", "/bin/mur", "agent", "cli", "a2", "--auto"]
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
            vec!["wezterm", "cli", "split-pane", "--right", "--", "/bin/mur", "agent", "cli", "a1"]
        );
        assert_eq!(
            cmds[1],
            vec!["wezterm", "cli", "split-pane", "--bottom", "--", "/bin/mur", "agent", "cli", "a2"]
        );
        assert_eq!(
            cmds[2],
            vec!["wezterm", "cli", "split-pane", "--right", "--", "/bin/mur", "agent", "cli", "a3"]
        );
    }

    #[test]
    fn kitty_plan_alternates_location() {
        let names = vec!["a1".to_string(), "a2".to_string()];
        let cmds = kitty_launches("/bin/mur", &names, false, false);
        assert_eq!(
            cmds[0],
            vec!["kitten", "@", "launch", "--location=vsplit", "--", "/bin/mur", "agent", "cli", "a1"]
        );
        assert_eq!(
            cmds[1],
            vec!["kitten", "@", "launch", "--location=hsplit", "--", "/bin/mur", "agent", "cli", "a2"]
        );
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-core multiplex`
Expected: COMPILE ERROR (functions not defined)

- [ ] **Step 3: Implement:**

```rust
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
            let loc = if i % 2 == 0 { "--location=vsplit" } else { "--location=hsplit" };
            let mut c: Vec<String> =
                vec!["kitten".into(), "@".into(), "launch".into(), loc.into(), "--".into()];
            c.extend(pane_argv(exe, name, resume, auto));
            c
        })
        .collect()
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core multiplex`
Expected: 11 passed

- [ ] **Step 5: Commit**

```bash
cargo clippy -p mur-core -- -D warnings && cargo fmt
git add mur-core/src/cmd/agent/cli/multiplex.rs
git commit -m "feat(agent-cli): zellij/wezterm/kitty pane planning"
```

---

### Task 5: Batch validation + executor (`multiplex::run`)

**Files:**
- Modify: `mur-core/src/cmd/agent/cli/multiplex.rs`

- [ ] **Step 1: Write the failing tests** — add inside `mod tests` (validation is filesystem-driven; use a temp `MUR_HOME`-style dir passed directly):

```rust
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
        let err = validate(home.path(), &["a1".into(), "a2".into()]).unwrap_err().to_string();
        assert!(err.contains("a2") && err.contains("mur agent run"), "got: {err}");
    }

    #[test]
    fn validate_canonicalizes_and_allows_duplicates() {
        let home = fake_home(&[("a1", true)]);
        let canon = validate(home.path(), &["A1".into(), "a1".into()]).unwrap();
        assert_eq!(canon, vec!["a1", "a1"]);
    }
```

Check `mur-core/Cargo.toml` `[dev-dependencies]` for `tempfile`; add `tempfile = "3"` there if absent (it is already used widely in this workspace).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p mur-core multiplex`
Expected: COMPILE ERROR (`validate` not defined)

- [ ] **Step 3: Implement `validate` and the real `run`/`execute`** (replace the Task 1 placeholder `run`):

```rust
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

use crate::a2a_dial::canonicalize_agent_name;

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
        bail!("unknown agent(s): {} — see `mur agent list`", unknown.join(", "));
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
        let name = if n == 1 { CHAT_LABEL.to_string() } else { format!("{CHAT_LABEL}-{n}") };
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
```

Keep all imports at the top of the file (merge with the placeholder's). If the file approaches 800 lines including tests, this is still within the limit (~600 expected); do NOT split.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p mur-core multiplex`
Expected: 14 passed

- [ ] **Step 5: Build and smoke-test the failure paths** (no multiplexer needed):

```bash
cargo build -p mur-core
./target/debug/mur agent cli definitely-missing also-missing; echo "exit=$?"
```
Expected: `Error: unknown agent(s): definitely-missing, also-missing — see `mur agent list``, non-zero exit.

- [ ] **Step 6: Commit**

```bash
cargo clippy -p mur-core -- -D warnings && cargo fmt
git add mur-core/src/cmd/agent/cli/multiplex.rs mur-core/Cargo.toml
git commit -m "feat(agent-cli): multi-agent split execution with batch validation"
```

---

### Task 6: `murmur` argv[0] dispatch

**Files:**
- Create: `mur-core/src/cli/murmur.rs`
- Modify: `mur-core/src/cli/mod.rs` (add `pub mod murmur;` next to the other mod declarations)
- Modify: `mur-core/src/main.rs` (`async_main`, currently `let cli = Cli::parse();` at ~line 120)

- [ ] **Step 1: Create `mur-core/src/cli/murmur.rs` with tests first** (tests + stubs in one file; the stubs return dummy values so tests compile but fail):

```rust
//! argv[0] dispatch for the `murmur` symlink — `murmur <names…>` is
//! shorthand for `mur agent cli <names…>` (BusyBox convention, same as
//! `mur_agent_<name>` → `mur-agent-runtime`).

use std::ffi::OsString;

/// True when the invoked binary's file stem is `murmur` (case-insensitive;
/// tolerates a Windows `.exe` suffix and any leading path).
pub fn is_murmur_invocation(argv0: Option<&OsString>) -> bool {
    let Some(a) = argv0 else { return false };
    std::path::Path::new(a)
        .file_stem()
        .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case("murmur"))
}

/// Rewrite murmur argv (`rest` excludes argv[0]) into a full
/// `mur agent cli …` argv for clap. When no positional agent name is
/// present: inject the concierge name `mur` if `concierge_exists`,
/// otherwise return `None` (caller prints the agent list and exits).
pub fn map_args(rest: &[OsString], concierge_exists: bool) -> Option<Vec<OsString>> {
    let has_name = rest.iter().any(|a| !a.to_string_lossy().starts_with('-'));
    let mut argv: Vec<OsString> =
        vec!["mur".into(), "agent".into(), "cli".into()];
    argv.extend(rest.iter().cloned());
    if !has_name {
        if !concierge_exists {
            return None;
        }
        argv.push("mur".into());
    }
    Some(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn detects_murmur_argv0_variants() {
        assert!(is_murmur_invocation(Some(&OsString::from("murmur"))));
        assert!(is_murmur_invocation(Some(&OsString::from("/opt/homebrew/bin/murmur"))));
        assert!(is_murmur_invocation(Some(&OsString::from("MURMUR.exe"))));
        assert!(!is_murmur_invocation(Some(&OsString::from("/opt/homebrew/bin/mur"))));
        assert!(!is_murmur_invocation(None));
    }

    #[test]
    fn maps_names_and_flags() {
        let argv = map_args(&os(&["a1", "a2", "--auto"]), false).unwrap();
        assert_eq!(argv, os(&["mur", "agent", "cli", "a1", "a2", "--auto"]));
    }

    #[test]
    fn no_name_injects_concierge_when_present() {
        let argv = map_args(&os(&["--resume"]), true).unwrap();
        assert_eq!(argv, os(&["mur", "agent", "cli", "--resume", "mur"]));
        let argv = map_args(&os(&[]), true).unwrap();
        assert_eq!(argv, os(&["mur", "agent", "cli", "mur"]));
    }

    #[test]
    fn no_name_no_concierge_returns_none() {
        assert!(map_args(&os(&[]), false).is_none());
    }
}
```

- [ ] **Step 2: Run tests** (logic above is written with the tests — verify they pass; if any fail, fix the implementation, not the test):

Run: `cargo test -p mur-core --bin mur murmur`
Expected: 4 passed. (If `pub mod murmur;` is missing from `mur-core/src/cli/mod.rs`, add it now — compile error otherwise.)

- [ ] **Step 3: Wire into `main.rs`.** In `async_main`, replace `let cli = Cli::parse();` with `let cli = parse_cli()?;` and add below `async_main`:

```rust
/// Parse argv, honoring the `murmur` symlink: `murmur <names…>` is
/// rewritten to `mur agent cli <names…>`. `murmur` with no agent name
/// falls back to the concierge agent, or lists agents and exits.
fn parse_cli() -> Result<Cli> {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if cli::murmur::is_murmur_invocation(args.first()) {
        let home = cmd::agent::resolve_mur_home()?;
        let concierge = home.join("agents").join("mur").join("profile.yaml").is_file();
        return match cli::murmur::map_args(&args[1..], concierge) {
            Some(argv) => Ok(Cli::parse_from(argv)),
            None => {
                eprintln!("murmur: no agent name given and no concierge agent installed.");
                eprintln!("Available agents:");
                let _ = cmd::agent::cmd_list(false);
                std::process::exit(2);
            }
        };
    }
    Ok(Cli::parse())
}
```

- [ ] **Step 4: End-to-end smoke test via a local symlink:**

```bash
cargo build -p mur-core
ln -sf "$PWD/target/debug/mur" /tmp/murmur
/tmp/murmur --help | head -5
```
Expected: help output for the `cli` subcommand (agent names + `--resume`/`--auto`), NOT the top-level mur help.

```bash
/tmp/murmur definitely-missing; echo "exit=$?"
```
Expected: the not-running/unknown-agent message from the single-name path (proves argv mapping reached `cmd_cli`).

- [ ] **Step 5: Commit**

```bash
cargo clippy -p mur-core -- -D warnings && cargo fmt
git add mur-core/src/cli/murmur.rs mur-core/src/cli/mod.rs mur-core/src/main.rs
git commit -m "feat: murmur symlink — argv[0] quick command for agent chat"
```

---

### Task 7: Install plumbing (build.sh + Homebrew formula)

**Files:**
- Modify: `build.sh` (install section, ~lines 53-65)
- Modify: `.github/workflows/release.yml` (formula heredoc, ~line 430)

- [ ] **Step 1: build.sh** — after the `sudo cp "$BINARY" /opt/homebrew/bin/mur` line, add:

```bash
  sudo ln -sfn /opt/homebrew/bin/mur /opt/homebrew/bin/murmur
  echo "Installed murmur -> /opt/homebrew/bin/mur (symlink)"
```

- [ ] **Step 2: release.yml formula** — inside `def install`, after `bin.install "mur-mcp-server"`:

```ruby
              bin.install_symlink "mur" => "murmur"
```
(Keep the heredoc's 10-space indentation — the workflow strips it with `sed 's/^          //'`.)

- [ ] **Step 3: Verify locally**

```bash
bash -n build.sh && echo "build.sh syntax OK"
```
Expected: `build.sh syntax OK`. (The release workflow only runs on tags; the formula line is verified by inspection + next release.)

- [ ] **Step 4: Commit**

```bash
git add build.sh .github/workflows/release.yml
git commit -m "build: install murmur symlink locally and via Homebrew"
```

---

### Task 8: Docs + final verification

**Files:**
- Modify: `README.md` (agent CLI usage section)
- Modify: `CLAUDE.md` (CLI surface bullet for `mur agent`)
- Modify: `docs/architecture/runtime-overview.md` (agent CLI section)

- [ ] **Step 1: CLAUDE.md** — in the `mur agent <subcommand>` bullet, extend the `cli` sentence to:

```
`cli <name>...` opens an interactive streaming TUI chat with a running agent (`--resume` to continue the last conversation); multiple names open one multiplexer pane per agent (tmux primary; zellij/WezTerm/kitty auto-detected). The `murmur` symlink is the quick form: `murmur a1 a2 a3` ≡ `mur agent cli a1 a2 a3`; bare `murmur` opens the concierge.
```

- [ ] **Step 2: README.md and runtime-overview.md** — find the existing `mur agent cli` mentions (`grep -n "agent cli" README.md docs/architecture/runtime-overview.md`) and add the same two facts in each place, matching the surrounding tone: (a) multiple names → split panes with backend auto-detection, (b) `murmur` quick command incl. bare-`murmur` concierge fallback. Include one example block:

```bash
murmur mur                 # chat with the concierge
murmur dev qa ops          # three agents, tiled panes (tmux/zellij/WezTerm/kitty)
mur agent cli dev qa       # long form, same behavior
```

- [ ] **Step 3: Full verification suite**

```bash
cargo fmt --check
cargo clippy --workspace -- -D warnings
cargo test -p mur-core multiplex
cargo test -p mur-core --bin mur murmur
cargo test -p mur-core --bin mur agent_cli
```
Expected: all green (14 + 4 + 3 tests).

- [ ] **Step 4: Manual E2E checklist** (needs real terminals; record results in the PR description):

1. Outside any multiplexer with tmux installed: `mur agent cli <a> <b>` → new `mur-chat` session, 2 tiled panes, both chats live; detach/reattach works; run again while first session exists → `mur-chat-2`.
2. Inside tmux: same command → new window in the current session, current window untouched.
3. `murmur <a> <b>` behaves identically; bare `murmur` opens concierge chat; `murmur --resume <a>` resumes.
4. (If available) WezTerm and kitty native splits; kitty with remote control off falls back to tmux.

- [ ] **Step 5: Commit docs**

```bash
git add README.md CLAUDE.md docs/architecture/runtime-overview.md
git commit -m "docs: agent cli multi-pane + murmur quick command"
```

- [ ] **Step 6: Follow-up (separate repo, not in this plan's commits):** update app.mur.run docs (`mur-server/dashboard/docs-content/` + `coreNavigation.tsx`) per the Documentation Checklist.

---

## Self-Review Notes

- Spec coverage: CLI surface (Task 1, 6), detection table rows 1-7 (Tasks 2, 5), error handling incl. batch validation + kitty fallback (Task 5), install/packaging (Task 7), docs (Task 8), unit + manual tests (all tasks + Task 8 Step 4).
- Spec extension flagged during planning: batch validation also rejects *stopped* agents (not just unknown ones) — otherwise a pane would flash its "not running" hint and close before the user can read it. Consistent with the spec's fail-early intent.
- `pane_shell`/`shell_quote` exist because tmux takes a single shell_command string while zellij/wezterm/kitty take argv directly — do not quote for the argv backends.
