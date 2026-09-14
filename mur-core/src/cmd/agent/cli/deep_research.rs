//! `/deep-research` murmur command: argv-safe subprocesses and live progress.

use std::path::Path;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use super::app::App;
use super::shell;
use super::stream::StreamMsg;
use crate::cmd::deep_research::status::DEFAULT_FLEET_NAME;
use crate::cmd::fleet::progress::{iteration_summary_line, load_view};

const TICK_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
pub enum DeepResearchAction {
    Status,
    Stop,
    Setup,
    Ask(String),
}

pub fn classify(args: &[String]) -> DeepResearchAction {
    match args {
        [] => DeepResearchAction::Status,
        [word] if word == "status" => DeepResearchAction::Status,
        [word] if word == "stop" => DeepResearchAction::Stop,
        [word] if word == "setup" => DeepResearchAction::Setup,
        _ => DeepResearchAction::Ask(args.join(" ")),
    }
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
    if app.shell.is_running() {
        app.push_system("a local command is already running — Ctrl-C to stop it");
        return;
    }

    let run_id = uuid::Uuid::now_v7().to_string();
    let (argv, display, ticker) = match action {
        DeepResearchAction::Status => (
            vec!["deep-research".to_string()],
            "mur deep-research".to_string(),
            false,
        ),
        DeepResearchAction::Stop => {
            app.push_system("kill-switch written — the loop exits at its next guard check");
            (
                vec![
                    "fleet".to_string(),
                    "stop".to_string(),
                    DEFAULT_FLEET_NAME.to_string(),
                ],
                format!("mur fleet stop {DEFAULT_FLEET_NAME}"),
                false,
            )
        }
        DeepResearchAction::Ask(question) => (
            vec!["deep-research".to_string(), question],
            "mur deep-research <question>".to_string(),
            true,
        ),
        DeepResearchAction::Setup => unreachable!("handled above"),
    };

    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            app.push_system(format!("could not locate mur executable: {error}"));
            return;
        }
    };
    let env = [("MUR_RUN_ID", run_id.as_str())];
    let (child, pid) = match shell::spawn_argv(&executable, &argv, &env).await {
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

    let (ticker_stop_tx, ticker_stop_rx) = oneshot::channel();
    if ticker {
        tokio::spawn(progress_ticker(
            app.home.clone(),
            run_id,
            tx.clone(),
            ticker_stop_rx,
        ));
    }

    let tx = tx.clone();
    tokio::spawn(async move {
        let end = shell::run(child, pid, gen_id, tx.clone(), cancel_rx).await;
        let _ = ticker_stop_tx.send(());
        let _ = tx.send(StreamMsg::ShellCardDone { gen_id, end }).await;
    });
}

async fn progress_ticker(
    home: std::path::PathBuf,
    run_id: String,
    tx: mpsc::Sender<StreamMsg>,
    mut stop: oneshot::Receiver<()>,
) {
    let mut last_iteration = None;
    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = tokio::time::sleep(TICK_INTERVAL) => {}
        }
        let Some(view) = load_matching_view(&home, &run_id) else {
            continue;
        };
        if !view.live {
            break;
        }
        let iteration = view.progress.iteration;
        if last_iteration == Some(iteration) {
            continue;
        }
        last_iteration = Some(iteration);
        if tx
            .send(StreamMsg::Note(iteration_summary_line(&view.progress)))
            .await
            .is_err()
        {
            break;
        }
    }
}

fn load_matching_view(
    home: &Path,
    run_id: &str,
) -> Option<crate::cmd::fleet::progress::ProgressView> {
    load_view(home, DEFAULT_FLEET_NAME).filter(|view| view.progress.run_id == run_id)
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
        assert_eq!(classify(&s(&["setup"])), Setup);
        assert_eq!(
            classify(&s(&["status", "of", "X"])),
            Ask("status of X".to_string())
        );
        assert_eq!(
            classify(&s(&["ask", "why", "now"])),
            Ask("ask why now".to_string())
        );
    }

    #[test]
    fn module_never_routes_research_output_to_the_agent() {
        let source = include_str!("deep_research.rs");
        assert!(!source.contains(&format!("{}{}", "finish_shell", "_turn")));
    }
}
