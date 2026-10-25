//! `/deep-research` murmur command: argv-safe subprocesses and live progress.

use std::path::Path;

use tokio::sync::{mpsc, oneshot};

use super::app::App;
use super::shell;
use super::stream::StreamMsg;
use crate::cmd::deep_research::status::DEFAULT_FLEET_NAME;

#[derive(Debug, PartialEq, Eq)]
pub enum DeepResearchAction {
    Status,
    Stop,
    Pause,
    Resume,
    Setup,
    Ask(String),
}

pub fn classify(args: &[String]) -> DeepResearchAction {
    match args {
        [] => DeepResearchAction::Status,
        [word] if word == "status" => DeepResearchAction::Status,
        [word] if word == "stop" => DeepResearchAction::Stop,
        [word] if word == "pause" => DeepResearchAction::Pause,
        [word] if word == "resume" => DeepResearchAction::Resume,
        [word] if word == "setup" => DeepResearchAction::Setup,
        [word] if word == "ask" => DeepResearchAction::Ask(String::new()),
        [word, rest @ ..] if word == "ask" => DeepResearchAction::Ask(rest.join(" ")),
        _ => DeepResearchAction::Ask(args.join(" ")),
    }
}

fn uses_shell_slot(action: &DeepResearchAction) -> bool {
    !matches!(
        action,
        DeepResearchAction::Stop
            | DeepResearchAction::Pause
            | DeepResearchAction::Resume
            | DeepResearchAction::Setup
    )
}

/// Dispatch without involving the conversational model. Commands use the
/// foreground shell-card slot so output, cancellation, and teardown behave
/// exactly like `!cmd`, while argv spawning keeps the question out of a shell.
pub(super) async fn handle(app: &mut App, args: &[String], tx: &mpsc::Sender<StreamMsg>) {
    let action = classify(args);
    if action == DeepResearchAction::Setup {
        app.push_system("run `mur deep-research setup` in a terminal — it asks for egress consent");
        return;
    }
    // The kill-switch is out-of-band control, not another foreground job.
    // It must remain usable precisely while the research child owns the slot.
    if action == DeepResearchAction::Stop {
        match crate::cmd::fleet::control::cmd_fleet_stop(&app.home, DEFAULT_FLEET_NAME) {
            Ok(()) => app.push_system(
                "stopping deep-research — in-flight member turns are cancelled and no further steps start",
            ),
            Err(error) => app.push_system(format!("could not stop deep-research: {error}")),
        }
        return;
    }
    // Pause is the same out-of-band sentinel write as stop: the running
    // research child sees it at the next iteration boundary, checkpoints,
    // and exits with outcome `paused`.
    if action == DeepResearchAction::Pause {
        match crate::cmd::fleet::pause::cmd_fleet_pause(&app.home, DEFAULT_FLEET_NAME) {
            Ok(()) => app.push_system(
                "pausing deep-research — in-flight member turns are cancelled and redone on resume; `/deep-research resume` continues it",
            ),
            Err(error) => app.push_system(format!("could not pause deep-research: {error}")),
        }
        return;
    }
    // Resume never takes the shell slot: it re-executes `mur fleet resume`
    // detached (stdin null, so an unanswered HITL gate defers) and the run's
    // output goes to its resume.log. The confirm window blocks for a few
    // seconds, so it runs off the UI task and reports back as a note.
    if action == DeepResearchAction::Resume {
        let executable = match std::env::current_exe() {
            Ok(path) => super::multiplex::canonical_mur_exe(path),
            Err(error) => {
                app.push_system(format!("could not locate mur executable: {error}"));
                return;
            }
        };
        app.push_system("resuming deep-research in the background…");
        let home = app.home.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                crate::cmd::fleet::loop_run::resume_detach::resume_detached(
                    &home,
                    DEFAULT_FLEET_NAME,
                    &executable,
                )
            })
            .await;
            let note = match result {
                Ok(Ok(message)) => message,
                Ok(Err(error)) => format!("could not resume deep-research: {error}"),
                Err(error) => format!("could not resume deep-research: {error}"),
            };
            let _ = tx.send(StreamMsg::Note(note)).await;
        });
        return;
    }
    if uses_shell_slot(&action) && app.shell.is_running() {
        app.push_system("a local command is already running — Ctrl-C to stop it");
        return;
    }

    let run_id = uuid::Uuid::now_v7().to_string();
    let is_research = matches!(&action, DeepResearchAction::Ask(_));
    let (argv, display) = match action {
        DeepResearchAction::Status => (
            vec!["deep-research".to_string()],
            "mur deep-research".to_string(),
        ),
        DeepResearchAction::Stop | DeepResearchAction::Pause | DeepResearchAction::Resume => {
            unreachable!("handled out of band above")
        }
        DeepResearchAction::Ask(question) if question.is_empty() => {
            app.push_system("usage: /deep-research ask <question>");
            return;
        }
        DeepResearchAction::Ask(question) => (
            vec!["deep-research".to_string(), question],
            "mur deep-research <question>".to_string(),
        ),
        DeepResearchAction::Setup => unreachable!("handled above"),
    };

    // `current_exe()` reports the `murmur` alias path when the chat was
    // launched that way, and `murmur deep-research <q>` expands to
    // `mur agent cli deep-research <q>` → "unknown agent(s): deep-research".
    // Normalize to the sibling `mur` binary so the subcommand dispatches.
    let executable = match std::env::current_exe() {
        Ok(path) => super::multiplex::canonical_mur_exe(path),
        Err(error) => {
            app.push_system(format!("could not locate mur executable: {error}"));
            return;
        }
    };
    let mut env = vec![("MUR_RUN_ID".to_string(), run_id.clone())];
    if let Some(size) = app.shell_term_size() {
        env.extend(size.env().map(|(k, v)| (k.to_string(), v)));
    }
    let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let (child, pid) = match shell::spawn_argv(&executable, &argv, &env_refs).await {
        Ok(value) => value,
        Err(error) => {
            app.begin_shell(&display);
            app.finish_shell(&shell::ShellEnd::SpawnFailed(error.to_string()));
            return;
        }
    };

    let (cancel_tx, cancel_rx) = oneshot::channel();
    let Some(gen_id) = app.shell.begin(pid, cancel_tx) else {
        shell::signal_group(pid, shell::SIGKILL_NUM);
        return;
    };
    app.begin_shell(&display);

    let tx = tx.clone();
    let home = app.home.clone();
    tokio::spawn(async move {
        let end = shell::run(child, pid, gen_id, tx.clone(), cancel_rx).await;
        if is_research && end == shell::ShellEnd::Cancelled {
            let _ = crate::cmd::fleet::control::cmd_fleet_stop(&home, DEFAULT_FLEET_NAME);
            mark_interrupted_stopped(&home, &run_id);
            let _ = tx
                .send(StreamMsg::Note(
                    "deep-research stopped — outcome: stopped".to_string(),
                ))
                .await;
        }
        let _ = tx.send(StreamMsg::ShellCardDone { gen_id, end }).await;
    });
}

fn mark_interrupted_stopped(home: &Path, run_id: &str) {
    let Some((mut progress, _)) = crate::cmd::fleet::progress::load(home, DEFAULT_FLEET_NAME)
    else {
        return;
    };
    if progress.run_id != run_id || progress.finished_at.is_some() {
        return;
    }
    progress.outcome = Some("stopped".to_string());
    progress.finished_at = Some(chrono::Utc::now().to_rfc3339());
    progress.save(home, DEFAULT_FLEET_NAME);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_string()).collect()
    }

    #[test]
    fn classify_reserved_words_only_when_sole() {
        use DeepResearchAction::*;
        assert_eq!(classify(&[]), Status);
        assert_eq!(classify(&s(&["status"])), Status);
        assert_eq!(classify(&s(&["stop"])), Stop);
        assert_eq!(classify(&s(&["pause"])), Pause);
        assert_eq!(classify(&s(&["resume"])), Resume);
        assert_eq!(
            classify(&s(&["pause", "the", "war"])),
            Ask("pause the war".to_string())
        );
        assert_eq!(classify(&s(&["setup"])), Setup);
        assert_eq!(
            classify(&s(&["status", "of", "X"])),
            Ask("status of X".to_string())
        );
        assert_eq!(
            classify(&s(&["ask", "why", "now"])),
            Ask("why now".to_string())
        );
    }

    #[test]
    fn stop_is_out_of_band_but_status_and_ask_use_the_shell_slot() {
        assert!(!uses_shell_slot(&DeepResearchAction::Stop));
        assert!(!uses_shell_slot(&DeepResearchAction::Pause));
        assert!(!uses_shell_slot(&DeepResearchAction::Resume));
        assert!(uses_shell_slot(&DeepResearchAction::Status));
        assert!(uses_shell_slot(&DeepResearchAction::Ask("q".into())));
    }

    #[test]
    fn interrupted_run_is_stamped_stopped() {
        let tmp = tempfile::tempdir().unwrap();
        let mut progress = crate::cmd::fleet::progress::RunProgress {
            schema_version: 1,
            run_id: "r-stop".into(),
            question: "q".into(),
            started_at: chrono::Utc::now().to_rfc3339(),
            finished_at: None,
            outcome: None,
            iteration: 2,
            model: None,
            budget_usd: Some(1.0),
            spend_usd: 0.25,
            billable: None,
            steps: vec![],
            artifact_path: None,
            error: None,
            paused_at: None,
        };
        progress.save(tmp.path(), DEFAULT_FLEET_NAME);

        mark_interrupted_stopped(tmp.path(), "r-stop");

        progress = crate::cmd::fleet::progress::load(tmp.path(), DEFAULT_FLEET_NAME)
            .unwrap()
            .0;
        assert_eq!(progress.outcome.as_deref(), Some("stopped"));
        assert!(progress.finished_at.is_some());
    }

    #[test]
    fn module_never_routes_research_output_to_the_agent() {
        let source = include_str!("deep_research.rs");
        assert!(!source.contains(&format!("{}{}", "finish_shell", "_turn")));
    }
}
