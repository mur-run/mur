//! Code-nav tip: when murmur runs inside a code workspace and this agent has
//! never run `mur code-nav setup`, say so once per session and offer the
//! setup command as a Tab chip. Nothing runs on its own — the chip is
//! insert-only (`ProposalKind::Shell`), so the user reviews the line first.
//!
//! Why here and not in the runtime: the runtime loads `mur-search`, but the
//! ladder's Symbol and Structure rungs need serena / ast-grep, which only
//! `mur code-nav setup` wires. Without it the skill silently falls back to
//! `rg`, and the user never learns the better tools exist.
//!
//! Language-agnostic by design: the signal is the working directory, not the
//! words in the prompt, because MUR users write in every language. The
//! marker list is read from the `mur-search` skill's own `session_start`
//! trigger, so the tip and the skill can never disagree about what a code
//! workspace is.

use mur_common::proposal::{Proposal, ProposalKind};
use mur_common::skill::TriggerKind;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use super::App;

/// `mur-search`'s `session_start.workspace_markers`, parsed once.
static MARKERS: LazyLock<Vec<String>> = LazyLock::new(|| {
    mur_common::skill::parse_canonical(include_str!("../../../skills/mur_search.yaml"))
        .ok()
        .and_then(|m| {
            m.triggers
                .into_iter()
                .find(|t| matches!(t.kind, TriggerKind::SessionStart))
                .map(|t| t.workspace_markers)
        })
        .unwrap_or_default()
});

const TIP: &str = "Tip: code navigation isn't set up for this agent, so code searches \
     fall back to plain `rg`. `mur code-nav setup` installs ast-grep and serena \
     (LSP symbol lookup) for this project. Press Tab to put the command in the \
     composer; review it, then send.";

const CHIP_LABEL: &str = "set up code navigation for this project";

/// The workspace root `cwd` sits in, if any.
pub(super) fn workspace_root(cwd: &Path) -> Option<PathBuf> {
    mur_common::repo_walk::marker_root_of(cwd, &MARKERS)
}

/// Whether `agent` has a code-nav setup manifest under `mur_home`.
pub(super) fn is_set_up(mur_home: &Path, agent: &str) -> bool {
    crate::cmd::code_nav::consent::manifest_path(mur_home, agent).is_file()
}

/// The command the chip inserts. `--yes` because murmur's `!command` has no
/// TTY; approving the chip line is the consent, as with `mur browser setup`.
/// Agent names are `[A-Za-z0-9_-]`; the project path is shell-quoted.
pub(super) fn setup_cmd(agent: &str, project: &Path) -> String {
    let project = super::multiplex::shell_quote(&project.to_string_lossy());
    format!("mur code-nav setup --yes --agent {agent} --with-serena --project {project}")
}

/// Called on each fresh turn. Considers the tip once per session, and never
/// covers a chip the agent already proposed.
pub(super) fn maybe_offer(app: &mut App) {
    if app.code_nav_tip_shown {
        return;
    }
    app.code_nav_tip_shown = true;
    let Some(root) = app.cwd.as_deref().and_then(workspace_root) else {
        return;
    };
    if is_set_up(&app.home, &app.agent) {
        return;
    }
    app.push_system(TIP);
    if app.proposal.as_ref().is_none_or(|p| p.is_reply()) {
        let cmd = setup_cmd(&app.agent, &root);
        super::proposal::offer(
            app,
            Proposal {
                label: CHIP_LABEL.into(),
                kind: ProposalKind::Shell(cmd),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_come_from_the_mur_search_skill() {
        assert!(MARKERS.iter().any(|m| m == ".git"), "{:?}", *MARKERS);
        assert!(MARKERS.iter().any(|m| m == "Cargo.toml"), "{:?}", *MARKERS);
    }

    #[test]
    fn workspace_root_is_the_nearest_marker_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(proj.join("src/deep")).unwrap();
        assert_eq!(workspace_root(&proj.join("src/deep")), None);
        std::fs::write(proj.join("package.json"), b"{}").unwrap();
        assert_eq!(workspace_root(&proj.join("src/deep")), Some(proj));
    }

    #[test]
    fn manifest_presence_decides_set_up() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!is_set_up(tmp.path(), "mur"));
        let p = crate::cmd::code_nav::consent::manifest_path(tmp.path(), "mur");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"{}").unwrap();
        assert!(is_set_up(tmp.path(), "mur"));
    }

    #[test]
    fn setup_cmd_names_agent_project_and_skips_the_tty_prompt() {
        assert_eq!(
            setup_cmd("mur", Path::new("/src/app")),
            "mur code-nav setup --yes --agent mur --with-serena --project /src/app"
        );
        let cmd = setup_cmd("mur", Path::new("/My Projects/app"));
        let argv = shell_words::split(&cmd).unwrap();
        assert_eq!(argv.last().unwrap(), "/My Projects/app", "{cmd}");
    }

    /// An `App` sitting in a fresh code workspace with code-nav not set up.
    fn app_in_workspace() -> (App, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("Cargo.toml"), b"").unwrap();
        let mut app = App::test_fixture();
        app.home = tmp.path().join("home");
        app.cwd = Some(proj);
        (app, tmp)
    }

    fn tip_count(app: &App) -> usize {
        app.messages.iter().filter(|m| m.text == TIP).count()
    }

    #[test]
    fn maybe_offer_tips_once_per_session_with_setup_chip() {
        let (mut app, tmp) = app_in_workspace();
        maybe_offer(&mut app);
        maybe_offer(&mut app);
        assert_eq!(tip_count(&app), 1);
        let p = app.proposal.as_ref().expect("setup chip offered");
        assert_eq!(p.label, CHIP_LABEL);
        assert_eq!(
            p.kind,
            ProposalKind::Shell(setup_cmd(&app.agent, &tmp.path().join("proj")))
        );
    }

    #[test]
    fn maybe_offer_keeps_an_existing_command_chip() {
        let (mut app, _tmp) = app_in_workspace();
        let theirs = Proposal {
            label: "theirs".into(),
            kind: ProposalKind::Shell("git status".into()),
        };
        app.proposal = Some(theirs.clone());
        maybe_offer(&mut app);
        assert_eq!(tip_count(&app), 1, "tip text still shown");
        assert_eq!(app.proposal, Some(theirs));
    }

    #[test]
    fn maybe_offer_replaces_a_reply_ghost() {
        let (mut app, _tmp) = app_in_workspace();
        app.proposal = Some(Proposal {
            label: String::new(),
            kind: ProposalKind::Reply("hi".into()),
        });
        maybe_offer(&mut app);
        let p = app.proposal.as_ref().unwrap();
        assert!(matches!(p.kind, ProposalKind::Shell(_)), "{p:?}");
    }

    #[test]
    fn maybe_offer_is_silent_outside_a_workspace_or_once_set_up() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::test_fixture();
        app.cwd = Some(tmp.path().to_path_buf());
        maybe_offer(&mut app);
        assert_eq!(tip_count(&app), 0);
        assert!(app.proposal.is_none());

        let (mut app, _tmp) = app_in_workspace();
        let m = crate::cmd::code_nav::consent::manifest_path(&app.home, &app.agent);
        std::fs::create_dir_all(m.parent().unwrap()).unwrap();
        std::fs::write(&m, b"{}").unwrap();
        maybe_offer(&mut app);
        assert_eq!(tip_count(&app), 0);
        assert!(app.proposal.is_none());
    }

    /// The keyword trigger must stay language-agnostic: code-shaped input
    /// fires it in any language; plain chat in any language does not.
    #[test]
    fn mur_search_keyword_trigger_is_code_shaped_only() {
        let m = mur_common::skill::parse_canonical(include_str!("../../../skills/mur_search.yaml"))
            .expect("mur-search parses");
        let rx = m
            .triggers
            .iter()
            .find(|t| matches!(t.kind, TriggerKind::Keyword))
            .and_then(|t| t.pattern.as_deref())
            .map(|p| regex::Regex::new(p).expect("trigger regex compiles"))
            .expect("mur-search has a keyword trigger");
        for p in [
            "幫我看 src/main.rs:42 這行",
            "¿Dónde se llama a parse_config()?",
            "この getUserName() はどこで定義？",
            "Warum schlägt mur_common::skill fehl?",
            "error[E0308]: mismatched types",
            "use ast grep 找 MUR",
        ] {
            assert!(rx.is_match(p), "should fire: {p}");
        }
        for p in [
            "hi",
            "今天天氣如何",
            "remind me at 10",
            "Bonjour, ça va ?",
            "幫我寫一封信",
        ] {
            assert!(!rx.is_match(p), "should not fire: {p}");
        }
    }
}
