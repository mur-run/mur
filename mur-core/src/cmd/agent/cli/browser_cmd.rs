//! `/browser` command: browser skill hub. `--add` attaches the built-in
//! `browser` skill to this agent; a mode argument (once attached) is sent to
//! the model as a turn.
//!
//! D5 (docs/superpowers/specs/2026-09-25-browser-skill-hub-design.md): this
//! variant is typed, never `SlashCmd::Unknown`, so `turn.rs`'s `submit()`
//! never routes it through `complete::matched_skill`'s fallthrough — that
//! guard is gated on `Unknown` and this dispatch arm has to do the whole job
//! itself, including the turn-start `matched_skill` would otherwise have done.

use mur_common::proposal::{Proposal, ProposalKind};

use super::*;

const SKILL_NAME: &str = "browser";

/// Step 2 of `--add`. Not run automatically: it downloads ~96 MiB and
/// widens the agent's spawn entitlements, so a human types `yes` for both.
/// `--yes` because there is no TTY inside murmur to type it on. Consent is
/// not skipped, it moves: the user approves this exact command as a chip,
/// and the spawn itself still passes the HITL gate.
///
/// `--agent` is load-bearing: the chip runs in a plain shell where
/// `MUR_AGENT` is unset, so without it `grant_perms` skips the grants and
/// the session agent never gets the spawn permissions. Agent names are
/// already `[A-Za-z0-9_-]` (`validate_agent_name`), so no shell quoting.
fn setup_cmd(agent: &str) -> String {
    format!("mur browser setup --yes --agent {agent}")
}

fn setup_hint(agent: &str) -> String {
    format!(
        "next: run `{}` — it installs Chromium (~96 MiB) and grants the spawn \
         permissions a browser run needs. Approving the command below is the \
         consent for both.",
        setup_cmd(agent)
    )
}

pub(super) async fn handle(app: &mut App, args: Vec<String>, tx: &mpsc::Sender<StreamMsg>) {
    use super::browser_live_cmd::{self as live, Route};
    let route = live::route(&args);
    if let Route::Live(hosts) = route {
        let ready = mur_browser::server::require_entry(Some(&app.home)).is_ok();
        run_manage(app, move |agent| {
            live::run(&agent, &hosts, ready, &mut live::ProfileOps(&agent))
        })
        .await;
        return;
    }
    // `--add` path is the pre-live behavior, kept byte-for-byte.
    if route == Route::Add {
        // D6: reuse `/skill add`'s own path unmodified. The global copy of
        // `mur-browser/SKILL.md` (shipped by `ensure_mur_skill`, D2/D3) is
        // the source `cmd_skill_add` parses; its `BUNDLE_ASSET_DIRS` copy
        // step then picks up `references/*.md` from the same directory —
        // no bespoke copy logic here.
        let source = app
            .home
            .join("skills")
            .join(SKILL_NAME)
            .join("SKILL.md")
            .to_string_lossy()
            .into_owned();
        run_manage(app, move |agent| manage::skill_add(&agent, &source)).await;
        // Step 2. `--add` attaches the skill and nothing else: the browser
        // download and the three spawn grants are privilege, and both live
        // behind the literal `yes` in `mur browser setup`. Point there
        // instead of granting silently — and offer it as a chip, so the
        // whole flow stays inside murmur.
        app.push_system(setup_hint(&app.agent));
        let cmd = setup_cmd(&app.agent);
        proposal::offer(
            app,
            Proposal {
                label: "install the browser and grant its permissions".into(),
                kind: ProposalKind::Shell(cmd),
            },
        );
        return;
    }

    if !app.skills.iter().any(|c| c.display == "/browser") {
        app.push_system("browser skill not attached — run /browser --add first");
        return;
    }

    let instruction = if args.is_empty() {
        "Use the browser skill.".to_string()
    } else {
        format!("Use the browser skill. {}", args.join(" "))
    };
    app.clear_input();
    start_turn(app, instruction, tx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_hint_and_chip_agree_and_pass_vet() {
        // The chip is the consent for both the download and the grants, so the
        // hint must name the same command the chip runs, and that command must
        // survive the same vet an agent's proposal does (no `<placeholder>`).
        let cmd = setup_cmd("mur");
        let hint = setup_hint("mur");
        assert!(hint.contains(&cmd), "{hint}");
        assert!(!hint.contains('<'), "{hint}");
        let args = serde_json::json!({
            "label": "install the browser and grant its permissions",
            "kind": "shell",
            "command": cmd,
        });
        assert!(mur_common::proposal::vet(&args).is_ok());
    }

    #[test]
    fn setup_cmd_names_the_session_agent() {
        // Regression: the chip runs in a plain shell with no MUR_AGENT, so
        // without `--agent` `grant_perms` printed "no agent given … skipping"
        // and the spawn grants never landed on the agent that ran `--add`.
        assert_eq!(setup_cmd("mur"), "mur browser setup --yes --agent mur");
    }
}
