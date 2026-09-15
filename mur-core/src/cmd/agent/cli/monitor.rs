//! `/monitor` (and `Ctrl+T`) murmur command: prints the same rows
//! `mur monitor list` prints, into the scrollback as a card. No overlay, no
//! alternate screen — the TUI's overlay path has a standing defect (a HITL
//! request is invisible outside `--plain`), and a status list isn't worth
//! inheriting it.

use tokio::sync::mpsc;

use super::app::App;
use super::stream::StreamMsg;
use mur_monitor::store::{ListFilter, MonitorStore};

pub(super) async fn handle(app: &mut App, _args: &[String], _tx: &mpsc::Sender<StreamMsg>) {
    let store = match MonitorStore::open(&app.home) {
        Ok(s) => s,
        Err(e) => {
            app.push_system(format!("monitor: {e:#}"));
            return;
        }
    };
    let rows = match store.list(&ListFilter::default()) {
        Ok(r) => r,
        Err(e) => {
            app.push_system(format!("monitor: {e:#}"));
            return;
        }
    };
    let mut buf: Vec<u8> = Vec::new();
    if let Err(e) = crate::cmd::monitor::render_list(&rows, &mut buf, chrono::Utc::now()) {
        app.push_system(format!("monitor: {e:#}"));
        return;
    }
    app.push_system(String::from_utf8_lossy(&buf).trim_end().to_string());
}
