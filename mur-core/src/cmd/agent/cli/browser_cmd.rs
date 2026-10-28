//! `/browser` command: browser skill hub. `--add` attaches the built-in
//! `browser` skill to this agent; `list` shows recorded runs; a mode argument
//! (once attached) is sent to the model as a turn, after the #2101 profile
//! pick and host pre-check in [`super::browser_mode_cmd`].
//!
//! D5 (docs/superpowers/specs/2026-09-25-browser-skill-hub-design.md): this
//! variant is typed, never `SlashCmd::Unknown`, so `turn.rs`'s `submit()`
//! never routes it through `complete::matched_skill`'s fallthrough — that
//! guard is gated on `Unknown` and this dispatch arm has to do the whole job
//! itself, including the turn-start `matched_skill` would otherwise have done.

use mur_common::proposal::{Proposal, ProposalKind};

use super::*;

const SKILL_NAME: &str = "browser";

/// Step 2 of `--add`. Not run automatically: it downloads
/// [`DOWNLOAD_SIZE`](crate::cmd::browser::setup::DOWNLOAD_SIZE) and
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
        "next: run `{}` — it installs Chromium ({}) and grants the spawn \
         permissions a browser run needs. Approving the command below is the \
         consent for both.",
        setup_cmd(agent),
        crate::cmd::browser::setup::DOWNLOAD_SIZE
    )
}

pub(super) async fn handle(app: &mut App, args: Vec<String>, tx: &mpsc::Sender<StreamMsg>) {
    use super::browser_live_cmd::{self as live, Route};
    use super::browser_mode_cmd as mode;
    let route = live::route(&args);
    if let Route::Live(hosts) = route {
        use super::browser_live_intent::{
            LiveIntent, TASK_NOTICE, classify_live_args, task_choices,
        };
        if classify_live_args(&hosts) == LiveIntent::Task {
            app.push_system(TASK_NOTICE);
            app.pending_suggestions = task_choices(&hosts);
            app.reveal_suggestions();
            return;
        }
        let ready = mur_browser::server::require_entry(Some(&app.home)).is_ok();
        // May pin + probe a new MCP entry (#1921): run off the event loop.
        run_manage_bg(
            app,
            "setting up live browser (probing)".into(),
            tx,
            move |agent| live::run(&agent, &hosts, ready, &mut live::ProfileOps(&agent)),
        );
        return;
    }
    if let Route::Allow(rest) = route {
        // 不用: remember it for this session; nothing else changes.
        if let [flag, host] = rest.as_slice()
            && flag == "--dismiss"
        {
            app.egress_chips.dismissed.insert(host.clone());
            app.push_system(format!(
                "ok — `{host}` stays blocked; no more prompts for it this session"
            ));
            return;
        }
        let home = app.home.clone();
        let agent = app.agent.clone();
        let args = rest.clone();
        let res = match tokio::task::spawn_blocking(move || {
            super::browser_allow_cmd::run_allow(
                &agent,
                &args,
                &mut super::browser_allow_cmd::ProfileAllowOps {
                    agent: &agent,
                    home,
                },
            )
        })
        .await
        {
            Ok(r) => r.map_err(|e| format!("error: {e:#}")),
            Err(e) => Err(format!("task failed: {e}")),
        };
        // Remembered so the pre-turn host check (#2101) does not ask again —
        // a session grant lives on the running proxy, not in the profile.
        // Only after it succeeded: a failed grant left the host blocked.
        if let Some(host) = super::browser_allow_cmd::granted_session_host(&rest, &res) {
            app.egress_chips.session_allowed.insert(host);
        }
        apply_managed(app, res);
        return;
    }
    if route == Route::Status {
        let home = app.home.clone();
        run_manage(app, move |agent| {
            super::browser_allow_cmd::run_status(&agent, &home)
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

    if route == Route::List {
        // Read-only and local: no skill or agent needed to see the table.
        match mode::parse_list(&args[1..]) {
            Ok(list_args) => {
                let runs = mur_browser::paths::browser_root(&app.home).join("runs");
                app.push_system(mode::render_list(&runs, &list_args, chrono::Utc::now()));
            }
            Err(e) => app.push_error(e),
        }
        return;
    }

    if !app.skills.iter().any(|c| c.display == "/browser") {
        app.push_system("browser skill not attached — run /browser --add first");
        return;
    }

    let saved = super::browser_profile_hint::load_saved_profiles(&app.home);
    let plan = match mode::plan_turn(&args, &saved) {
        Ok(p) => p,
        Err(e) => {
            app.push_error(e);
            return;
        }
    };
    // #2101 item 4: ask before the turn, not after the first navigation fails.
    let entry = crate::cmd::agent::load_profile_for_edit(&app.agent)
        .ok()
        .and_then(|(_, p)| {
            p.mcp_servers
                .into_iter()
                .find(|e| e.name == live::LIVE_ENTRY)
        });
    let blocked = mode::blocked_hosts(
        &mode::url_hosts(&plan.args),
        entry.as_ref(),
        &app.egress_chips.dismissed,
        &app.egress_chips.session_allowed,
    );
    if let Some((text, chips)) = mode::allow_prompt(&blocked) {
        app.push_system(text);
        app.pending_suggestions = chips;
        app.reveal_suggestions();
        return;
    }
    if let Some(note) = &plan.note {
        app.push_system(note.clone());
    }
    app.clear_input();
    start_turn(app, mode::instruction(&plan), tx);
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
