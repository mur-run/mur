//! Keys for the docked cards. An open HITL gate is handled before this and
//! outranks every card.

use tokio::sync::mpsc;

use super::app::App;
use super::ssh_card;
use super::stream::StreamMsg;

/// Route `key` to an open card. `true` means a card owned it. Ctrl combos
/// (cancel, quit) still reach the composer's handlers.
pub fn on_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    ctrl: bool,
    tx: &mpsc::Sender<StreamMsg>,
) -> bool {
    // The `/ssh` card owns the keys while open.
    if app.ssh.card.is_some() && !ctrl {
        if app.ssh.settled() {
            ssh_card::on_key(app, key, tx);
        }
        return true;
    }
    false
}
