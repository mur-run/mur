//! Slash-command handling, moved out of `mod.rs` for CLAUDE.md §4's
//! 800-line rule. Pure movement: verbatim.

use super::*;

/// Why a `/channels <target>` lookup found nothing usable.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ResolveErr {
    NotFound,
    /// An id prefix matched more than one channel; carries the short ids.
    Ambiguous(Vec<String>),
    /// An id prefix so short it would match almost anything.
    TooShort,
}

/// Shortest id prefix we will act on. Below this a typo silently lands you in
/// someone else's conversation, so we ask for more characters instead.
const MIN_ID_PREFIX: usize = 4;

/// Resolve what the user typed after `/channels` against the recent list.
pub(super) fn resolve<'a>(
    recent: &'a [persist::SessionInfo],
    target: &ChannelRef,
) -> Result<&'a persist::SessionInfo, ResolveErr> {
    match target {
        ChannelRef::Ordinal(n) => recent
            .iter()
            .find(|s| s.ordinal == *n)
            .ok_or(ResolveErr::NotFound),
        ChannelRef::IdPrefix(p) => {
            if p.len() < MIN_ID_PREFIX {
                return Err(ResolveErr::TooShort);
            }
            let hits: Vec<&persist::SessionInfo> = recent
                .iter()
                .filter(|s| s.id.to_ascii_lowercase().starts_with(p))
                .collect();
            match hits.len() {
                0 => Err(ResolveErr::NotFound),
                1 => Ok(hits[0]),
                _ => Err(ResolveErr::Ambiguous(
                    hits.iter()
                        .map(|s| s.id[..s.id.len().min(8)].to_string())
                        .collect(),
                )),
            }
        }
    }
}

/// One-line rendering of a failed lookup, for the system pane.
pub(super) fn resolve_msg(target: &ChannelRef, err: &ResolveErr) -> String {
    let what = match target {
        ChannelRef::Ordinal(n) => format!("channel {n}"),
        ChannelRef::IdPrefix(p) => format!("channel id starting {p}"),
    };
    match err {
        ResolveErr::NotFound => format!("no {what} — /channels to list"),
        ResolveErr::TooShort => {
            format!("{what} is too short — give at least {MIN_ID_PREFIX} characters")
        }
        ResolveErr::Ambiguous(ids) => {
            format!("{what} matches {} — try a longer prefix", ids.join(", "))
        }
    }
}

pub(super) async fn handle_slash(app: &mut App, cmd: SlashCmd, tx: &mpsc::Sender<StreamMsg>) {
    match cmd {
        SlashCmd::Help => app.push_system(help_text()),
        SlashCmd::Quit => request_quit(app, tx),
        SlashCmd::Clear => {
            // Stop the in-flight turn first so its worker can't write into the
            // fresh conversation after the reset. Same reason for a running
            // `!cmd` (D8): otherwise the process runs on invisibly and its
            // report lands in the new conversation.
            cancel_in_flight(app, tx);
            stop_shell(app, false);
            match Session::create(&app.home, &app.agent) {
                Ok(s) => app.start_new_session(s),
                Err(e) => app.push_system(format!("could not start new session: {e}")),
            }
        }
        SlashCmd::Sessions => match persist::list_recent(&app.home, &app.agent, RECENT_LIMIT) {
            Ok(list) if !list.is_empty() => {
                let mut out = String::from(
                    "recent conversations (resume the latest with `mur agent cli --resume`):\n",
                );
                for s in list {
                    out.push_str(&format!(
                        "  {} · {} turns · {}\n",
                        &s.id[..s.id.len().min(8)],
                        s.turns,
                        s.preview
                    ));
                }
                app.push_system(out.trim_end().to_string());
            }
            Ok(_) => app.push_system("no saved conversations yet"),
            Err(e) => app.push_system(format!("could not list sessions: {e}")),
        },
        SlashCmd::Channels { target, follow } => {
            // `--follow` never touches the current conversation: it tails
            // ANOTHER channel while this pane keeps chatting, so an in-flight
            // turn must not be cancelled for it.
            if follow {
                let recent = match persist::list_recent(&app.home, &app.agent, RECENT_LIMIT) {
                    Ok(r) => r,
                    Err(e) => {
                        app.push_system(format!("could not list channels: {e}"));
                        return;
                    }
                };
                match target {
                    None => {
                        match app.follow.take() {
                            Some(f) => app.push_system(format!("stopped following {}", f.tag())),
                            None => app.push_system(
                                "not following anything — /channels N --follow to start",
                            ),
                        }
                        return;
                    }
                    Some(t) => match resolve(&recent, &t) {
                        Ok(s) => {
                            let id = s.id.clone();
                            match app.start_follow(&id, StdInstant::now()) {
                                Ok(()) => app.push_system(format!(
                                    "following {} — new events appear here; /channels --follow to stop",
                                    &id[..id.len().min(8)]
                                )),
                                Err(e) => app.push_system(format!("could not follow: {e:#}")),
                            }
                        }
                        Err(e) => app.push_system(resolve_msg(&t, &e)),
                    },
                }
                return;
            }
            // Cancel any in-flight stream before we potentially switch channels.
            if app.streaming {
                if let Some(tid) = app.current_task_id.clone() {
                    let _ = cancel_task(app.home.clone(), app.agent.clone(), tid).await;
                }
                app.finish_partial();
            }
            let recent = match persist::list_recent(&app.home, &app.agent, RECENT_LIMIT) {
                Ok(r) => r,
                Err(e) => {
                    app.push_system(format!("could not list channels: {e}"));
                    return;
                }
            };
            match target {
                Some(t) => match resolve(&recent, &t) {
                    Ok(s) => {
                        let id = s.id.clone();
                        stop_shell(app, false);
                        match app.switch_channel(&id) {
                            Ok(()) => app.push_system(format!(
                                "switched to channel {} ({} turns)",
                                &id[..id.len().min(8)],
                                app.messages
                                    .iter()
                                    .filter(|m| matches!(m.role, Role::User | Role::Agent))
                                    .count()
                            )),
                            Err(e) => app.push_system(format!("could not switch channel: {e}")),
                        }
                    }
                    Err(e) => app.push_system(resolve_msg(&t, &e)),
                },
                None => {
                    if recent.is_empty() {
                        app.push_system("no channels yet");
                    } else {
                        let mut out = String::from(
                            "channels (/channels N or id-prefix to switch · add --follow to tail):\n",
                        );
                        for s in recent.iter() {
                            out.push_str(&format!(
                                "  {} · {} · {} turns · {}\n",
                                s.ordinal,
                                &s.id[..s.id.len().min(8)],
                                s.turns,
                                s.preview
                            ));
                        }
                        app.push_system(out.trim_end().to_string());
                    }
                }
            }
        }
        SlashCmd::Card => {
            let (h, a) = (app.home.clone(), app.agent.clone());
            let res = tokio::task::spawn_blocking(move || {
                dial_method(&h, &a, "agent/card", Value::Null, DialMode::Auto)
            })
            .await;
            match res {
                Ok(Ok(card)) => app.push_system(
                    serde_json::to_string_pretty(&card).unwrap_or_else(|_| card.to_string()),
                ),
                Ok(Err(e)) => app.push_system(format!("card error: {e:#}")),
                Err(e) => app.push_system(format!("card task failed: {e}")),
            }
        }
        SlashCmd::Auto(set) => {
            app.auto_approve = set.unwrap_or(!app.auto_approve);
            if app.auto_approve {
                app.push_system(
                    "auto-approve ON — every tool call is allowed without asking (this session only; /auto off to disable)",
                );
                // A prompt may already be waiting — resolve it under the new mode.
                if app.hitl.is_some() {
                    decide_hitl_with_note(app, tx, true, true);
                }
            } else {
                // Revoke the per-tool `[a]` grants too. To the operator those
                // ARE auto-approval, so leaving them behind made "tool calls
                // ask again" a lie — and nothing else ever cleared
                // `session_tool_allow`, so a grant (including one pressed by
                // accident) was irreversible for the rest of the session.
                let mut revoked: Vec<String> = app.session_tool_allow.drain().collect();
                revoked.sort_unstable();
                if revoked.is_empty() {
                    app.push_system("auto-approve OFF — tool calls ask again");
                } else {
                    app.push_system(format!(
                        "auto-approve OFF — tool calls ask again (revoked the session allow for {})",
                        revoked.join(", ")
                    ));
                }
            }
        }
        SlashCmd::Verbose(set) => {
            app.cards_expanded = set.unwrap_or(!app.cards_expanded);
            if app.cards_expanded {
                app.push_system(
                    "verbose ON — tool cards show full args + result (Ctrl+O for the transcript any time)",
                );
            } else {
                app.push_system("verbose OFF — tool cards collapse to a one-line summary");
            }
        }
        SlashCmd::Model(arg) => {
            let reg = mur_common::model::ModelRegistry::default_path()
                .and_then(|p| mur_common::model::ModelRegistry::load_from(&p));
            let reg = match reg {
                Ok(r) => r,
                Err(e) => {
                    app.push_system(format!("model registry unavailable: {e:#}"));
                    return;
                }
            };
            let models = model_cmd::ordered_models(&reg);
            match arg {
                None => {
                    let cur = model_cmd::current_model_ref(&app.home, &app.agent);
                    app.push_system(model_cmd::render_list(&models, cur.as_deref()));
                }
                Some(a) => {
                    let Some(target) = model_cmd::resolve_pick(&models, &a) else {
                        app.push_system(format!("no such model: {a} — /model to list"));
                        return;
                    };
                    // Dual-write, profile first: this process owns the file,
                    // the sealed runtime does not (its launch chain denies the
                    // write — the profile is the operator's), so `model/set`
                    // only swaps the live client. Disk first means the pick
                    // survives a runtime that cannot switch (old build, echo
                    // agent, not running): a restart applies it.
                    if let Err(w) = model_cmd::write_model_ref(&app.home, &app.agent, &target) {
                        app.push_system(format!(
                            "model switch failed: profile write failed: {w:#}"
                        ));
                        return;
                    }
                    let (h, ag, mref) = (app.home.clone(), app.agent.clone(), target.clone());
                    let res = tokio::task::spawn_blocking(move || {
                        dial_method(
                            &h,
                            &ag,
                            "model/set",
                            serde_json::json!({ "model_ref": mref }),
                            DialMode::Auto,
                        )
                    })
                    .await;
                    match res {
                        Ok(Ok(_)) => app.push_system(format!(
                            "model → {target} (effective next turn; saved to profile)"
                        )),
                        Ok(Err(e)) => app.push_system(format!(
                            "saved {target} to profile; couldn't hot-switch ({e:#}) — restart the agent to apply"
                        )),
                        Err(e) => app.push_system(format!(
                            "saved {target} to profile; model task failed: {e} — restart the agent to apply"
                        )),
                    }
                }
            }
        }
        SlashCmd::Effort { level, save } => {
            // The levels on offer are a property of the model this agent is
            // configured with, so resolve it rather than listing the whole
            // scale. `provider:` is the wire protocol, never the vendor — the
            // raw id in `ModelEntry.model` is what the table keys on.
            let model_id = model_cmd::current_model_id(&app.home, &app.agent).unwrap_or_default();
            let levels = mur_common::llm::effort_shape(&model_id).levels();

            if levels.is_empty() {
                app.push_system(format!(
                    "{model_id} takes no reasoning effort parameter — nothing to set"
                ));
                return;
            }

            let profile_effort = model_cmd::current_effort(&app.home, &app.agent);
            let Some(raw) = level else {
                let (eff, src) = mur_common::llm::effective_effort(
                    app.session_effort,
                    profile_effort,
                    &model_id,
                );
                let offered: Vec<&str> = levels.iter().map(|e| e.as_str()).collect();
                let now = match (eff, src) {
                    (Some(e), mur_common::llm::EffortSource::SessionOverride) => {
                        format!("{} (this session)", e.as_str())
                    }
                    (Some(e), _) => format!("{} (profile)", e.as_str()),
                    (None, _) => "unset — the API default is high".to_string(),
                };
                app.push_system(format!(
                    "{model_id} accepts: {}\ncurrent: {now}\n/effort <level> for this session, --save to persist",
                    offered.join(" · ")
                ));
                return;
            };

            let want: mur_common::llm::Effort = match raw.parse() {
                Ok(e) => e,
                Err(e) => {
                    app.push_system(e.to_string());
                    return;
                }
            };
            // Report what the model will ACTUALLY use. The runtime stores what
            // was asked for and each client narrows at the wire, so a level
            // this model lacks is not an error — but saying nothing about it
            // would leave the user believing a setting that never applied.
            let (applied, _) = mur_common::llm::effective_effort(Some(want), None, &model_id);

            let (h, ag) = (app.home.clone(), app.agent.clone());
            let lvl = want.as_str();
            let res = tokio::task::spawn_blocking(move || {
                dial_method(
                    &h,
                    &ag,
                    "effort/set",
                    serde_json::json!({ "level": lvl }),
                    DialMode::Auto,
                )
            })
            .await;

            let narrowed = applied.filter(|a| *a != want);
            let suffix = match narrowed {
                Some(a) => format!(
                    " (this model has no {}; using {})",
                    want.as_str(),
                    a.as_str()
                ),
                None => String::new(),
            };
            match res {
                Ok(Ok(_)) => {
                    app.session_effort = Some(want);
                    app.push_system(format!(
                        "effort → {}{suffix} (this session, effective next turn)",
                        want.as_str()
                    ));
                }
                Ok(Err(e)) => app.push_system(format!(
                    "couldn't set effort on the running agent ({e:#}){suffix}"
                )),
                Err(e) => app.push_system(format!("effort task failed: {e}")),
            }

            if save {
                match crate::cmd::agent::cmd_effort(
                    &app.agent,
                    Some(want.as_str().to_string()),
                    false,
                ) {
                    Ok(()) => app.push_system(format!("saved {} to profile", want.as_str())),
                    Err(e) => app.push_system(format!("profile write failed: {e:#}")),
                }
            }
        }
        SlashCmd::Login(arg) => match arg {
            None => run_manage(app, move |agent| Ok(login::render_status_all(&agent))).await,
            Some(word) => match login::Provider::parse(&word) {
                None => app.push_error(format!(
                    "unknown provider {word:?} — try anthropic or chatgpt"
                )),
                Some(p) => login::dispatch_repair(app, p).await,
            },
        },
        SlashCmd::Secret { key, delete } => secret_cmd::request(app, key, delete),
        SlashCmd::Mcp(args) => run_manage(app, move |agent| manage::run_mcp(&agent, &args)).await,
        SlashCmd::Skill(args) => {
            run_manage(app, move |agent| manage::run_skill(&agent, &args)).await
        }
        SlashCmd::Remember(args) => match memory_cmds::remember(&app.home, &app.agent, &args) {
            Ok(msg) => {
                let note = push_memory_reload(&app.home, &app.agent).await;
                app.push_system(format!("{msg}{note}"));
            }
            Err(e) => app.push_system(format!("remember failed: {e}")),
        },
        SlashCmd::Memories => app.push_system(memory_cmds::memories(&app.home, &app.agent)),
        SlashCmd::Forget(target) => {
            match memory_cmds::forget(&app.home, &app.agent, target.as_deref()) {
                Ok(msg) => {
                    let note = push_memory_reload(&app.home, &app.agent).await;
                    app.push_system(format!("{msg}{note}"));
                }
                Err(e) => app.push_system(format!("forget failed: {e}")),
            }
        }
        SlashCmd::Skin(name_opt) => match name_opt {
            None => {
                let current = theme::skin_name(app.theme);
                app.push_system(format!(
                    "current skin: {current} — valid: {}",
                    theme::SKIN_NAMES
                ));
            }
            Some(name) => {
                if !theme::is_known_skin(&name) {
                    app.push_system(format!(
                        "unknown skin '{name}' — valid: {}",
                        theme::SKIN_NAMES
                    ));
                } else {
                    app.theme = theme::resolve_skin(&name);
                    app.mascot_mode =
                        welcome::resolve_mascot_mode(app.theme, std::io::stdout().is_terminal());
                    let h = app.home.clone();
                    match persist_skin(&h, &name) {
                        Ok(()) => app.push_system(format!("skin changed to {name}")),
                        Err(e) => app.push_system(format!(
                            "skin changed to {name} (could not persist: {e})"
                        )),
                    }
                }
            }
        },
        SlashCmd::Panel(args) => panel::handle_panel_command(app, &args),
        SlashCmd::Open => {
            let items = crate::open_items::collect(&app.home);
            let (visible, muted) = crate::open_items::partition(items, &app.muted_origins());
            let (visible, stale) = crate::open_items::split_stale(visible, chrono::Utc::now());
            app.open_items_fp = Some(crate::open_items::fingerprint(&visible));
            app.push_system(
                crate::open_items::render(&visible, &muted, stale.len())
                    .trim()
                    .to_string(),
            );
        }
        SlashCmd::DeepResearch(args) => deep_research::handle(app, &args, tx).await,
        SlashCmd::Search(args) => search::handle(app, &args, tx).await,
        SlashCmd::Monitor(args) => monitor::handle(app, &args, tx).await,
        SlashCmd::Unknown(c) => app.push_system(format!("unknown command: /{c} — try /help")),
    }
}

/// Run a blocking profile-management closure off the event loop and render
/// its outcome as a system note.
pub(super) async fn run_manage<F>(app: &mut App, f: F)
where
    F: FnOnce(String) -> Result<String> + Send + 'static,
{
    let agent = app.agent.clone();
    match tokio::task::spawn_blocking(move || f(agent)).await {
        Ok(Ok(text)) => app.push_system(text),
        Ok(Err(e)) => app.push_error(format!("error: {e:#}")),
        Err(e) => app.push_error(format!("task failed: {e}")),
    }
}

/// Replay the current turn's `message/send` after a dial failure, under a
/// fresh client task id: the runtime keys tasks by it, and the old id may be
/// half-registered on the peer that just died.
pub(super) fn retry_send(app: &mut App, mut params: Value, tx: &mpsc::Sender<StreamMsg>) {
    let task_id = uuid::Uuid::now_v7().to_string();
    params["task_id"] = Value::String(task_id.clone());
    app.current_task_id = Some(task_id.clone());
    app.inflight_params = Some(params.clone());
    spawn_stream(
        app.home.clone(),
        app.agent.clone(),
        params,
        task_id,
        tx.clone(),
    );
}

/// `/channels <target>` resolution: an ordinal names exactly one channel for
/// life, and an id prefix must be unambiguous before we act on it.
#[cfg(test)]
mod resolve_tests {
    use super::{ChannelRef, ResolveErr, resolve};
    use crate::cmd::agent::cli::persist::SessionInfo;

    fn si(id: &str, ordinal: u64) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            preview: String::new(),
            turns: 1,
            ordinal,
        }
    }

    fn recent() -> Vec<SessionInfo> {
        // Newest-first, so list position and ordinal deliberately disagree.
        vec![
            si("01a0d420beef", 7),
            si("01a0d999cafe", 2),
            si("0bbb1111", 5),
        ]
    }

    #[test]
    fn an_ordinal_matches_the_number_not_the_list_position() {
        let r = recent();
        assert_eq!(resolve(&r, &ChannelRef::Ordinal(2)).unwrap().id, r[1].id);
        assert_eq!(resolve(&r, &ChannelRef::Ordinal(7)).unwrap().id, r[0].id);
    }

    #[test]
    fn an_unknown_ordinal_is_not_found() {
        assert!(matches!(
            resolve(&recent(), &ChannelRef::Ordinal(99)),
            Err(ResolveErr::NotFound)
        ));
    }

    #[test]
    fn an_id_prefix_resolves_when_it_is_unique() {
        assert_eq!(
            resolve(&recent(), &ChannelRef::IdPrefix("01a0d420".into()))
                .unwrap()
                .ordinal,
            7
        );
    }

    /// Switching to the wrong conversation is silent and confusing, so a
    /// prefix shared by two channels refuses rather than picking one.
    #[test]
    fn a_shared_id_prefix_is_ambiguous_and_names_the_candidates() {
        match resolve(&recent(), &ChannelRef::IdPrefix("01a0d".into())) {
            Err(ResolveErr::Ambiguous(ids)) => {
                assert_eq!(ids, vec!["01a0d420".to_string(), "01a0d999".to_string()]);
            }
            other => panic!("expected ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn a_too_short_id_prefix_is_refused_before_matching() {
        assert!(matches!(
            resolve(&recent(), &ChannelRef::IdPrefix("01".into())),
            Err(ResolveErr::TooShort)
        ));
    }
}
