//! Fixtures shared by the `/review` UI tests (`state_tests`, `start_tests`).

use std::path::Path;
use std::time::Duration;

use tokio::sync::mpsc::{self, Receiver};

use crate::cmd::agent::cli::Role;
use crate::cmd::agent::cli::app::App;
use crate::cmd::agent::cli::persist::Session;
use crate::cmd::agent::cli::stream::{STREAM_CHANNEL_CAP, StreamMsg};
use crate::cmd::fleet::review::constants::RUNNING_LOCK;
use crate::cmd::fleet::review::murmur::bridge::DriverReq;

/// Long enough for a loaded CI box; the worker answers in milliseconds.
const WAIT: Duration = Duration::from_secs(20);

/// A home that outlives the test body (`app/tests/state.rs::app()` drops its
/// tempdir, which is fine for it but would delete the channel we need).
pub(super) fn home() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    crate::channel_writer::plant_writer_identity(tmp.path());
    tmp
}

pub(super) fn app_at(home: &Path) -> App {
    let session = Session::create(home, "a").unwrap();
    App::new(
        home.to_path_buf(),
        "a".into(),
        session,
        &crate::cmd::agent::cli::theme::ANSI,
    )
}

/// An agent directory `canonicalize_agent_name` recognises; `running` plants
/// the lock file `require_running` checks.
pub(super) fn agent(home: &Path, name: &str, running: bool) {
    let dir = home.join("agents").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("profile.yaml"), format!("name: {name}\n")).unwrap();
    if running {
        std::fs::write(dir.join(RUNNING_LOCK), "{}").unwrap();
    }
}

pub(super) fn system_lines(app: &App) -> Vec<&str> {
    app.messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.text.as_str())
        .collect()
}

/// A sender whose receiver is dropped: for tests that never read the stream.
pub(super) fn tx() -> mpsc::Sender<StreamMsg> {
    mpsc::channel(8).0
}

/// A sender and the receiver the test reads the worker's requests from.
pub(super) fn stream() -> (mpsc::Sender<StreamMsg>, mpsc::Receiver<StreamMsg>) {
    mpsc::channel(STREAM_CHANNEL_CAP)
}

pub(super) fn fleets(home: &Path) -> Vec<String> {
    crate::cmd::fleet::store::list_fleets(home).unwrap()
}

/// The next message, skipping transcript `Show`s.
pub(super) async fn next(rx: &mut Receiver<StreamMsg>) -> StreamMsg {
    loop {
        let msg = tokio::time::timeout(WAIT, rx.recv())
            .await
            .expect("the worker answered in time")
            .expect("the stream stays open until Finished");
        if !matches!(msg, StreamMsg::ReviewReq(DriverReq::Show(_))) {
            return msg;
        }
    }
}
