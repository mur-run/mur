//! Pre-seal read of the browser profile state key (#2369).
//!
//! A live-browser MCP child (`mur browser record --mode live --profile …`)
//! decrypts its saved profile at startup and re-encrypts the rotated session
//! at exit with an age identity kept in the OS Keychain. Once the sandbox
//! seals, the Keychain is unreachable from this process and every child, so
//! the key is read here and cached; `protocol::mcp_client::state_key` hands
//! it to the matching child over an inherited pipe.
//!
//! Same rules as the user secrets in `seal.rs`: bounded on its own thread (an
//! unauthorised item raises a modal prompt that blocks until clicked), and a
//! timed-out prompt is recorded with `keychain_pending` so a crash loop does
//! not raise it again on every start.

use super::bootstrap::USER_SECRET_RESOLVE_TIMEOUT;
use mur_common::agent::McpServerEntry;
use mur_common::browser_state_key::{PENDING_NAME, keychain_ref, wants_state_key};
use mur_common::secret::keychain_pending;
use std::path::Path;
use tracing::{info, warn};

/// Cache the key before the seal when any enabled server is a live browser
/// bound to a profile. Never fails startup: a miss leaves the child to try the
/// Keychain itself, which is exactly the behaviour before this existed.
pub(super) fn cache_before_seal(entries: &[McpServerEntry], agent_home: &Path, agent: &str) {
    if !entries.iter().any(|e| wants_state_key(&e.args)) {
        return;
    }
    if keychain_pending::is_pending(agent_home, PENDING_NAME) {
        warn!(
            agent,
            "skipping the browser state key: an earlier start is still waiting for \
             keychain approval. Choose Always Allow, then run `mur agent restart {agent}` \
             (or `mur agent start {agent}` if it is stopped)"
        );
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(mur_common::secret::cache_before_seal(&keychain_ref()));
    });
    match rx.recv_timeout(USER_SECRET_RESOLVE_TIMEOUT) {
        Ok(Ok(())) => info!("browser state key resolved before sandbox seal"),
        Ok(Err(e)) => warn!(
            error = %e,
            "could not read the browser state key before sealing; a live browser with \
             a saved profile will fail to decrypt it — run `mur browser auth <site>` \
             outside the agent first"
        ),
        Err(_) => {
            warn!(
                timeout_secs = USER_SECRET_RESOLVE_TIMEOUT.as_secs(),
                agent,
                "keychain did not answer for the browser state key in time — most likely \
                 an authorisation prompt nobody is there to click. Choose Always Allow and \
                 run `mur agent restart {agent}`"
            );
            if let Err(e) = keychain_pending::mark(agent_home, PENDING_NAME) {
                warn!(error = %e, "could not record the pending keychain prompt");
            }
        }
    }
}
