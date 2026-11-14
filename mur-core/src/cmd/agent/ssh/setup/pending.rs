//! Pending `ssh_request`s as every client lists them (SSH setup spec,
//! "Unattended requests"): murmur's `/ssh`, Hub's SSH block and Hub's
//! "Needs you" read the same list. Moved from murmur's `ssh_card` (#2370).

use std::path::Path;

use chrono::Utc;
use mur_ssh_request::{PendingRequest, RequestHost, read_pending};

use super::super::status::{self, PairState, PairStatus, RequestStatus};

/// A pending request that is still open, and whether its host already has a
/// live grant (approved or signed) so the card has nothing left to do.
#[derive(Debug, Clone)]
pub struct OpenRequest {
    pub request: PendingRequest,
    pub granted: bool,
}

/// The pending requests a signing mark has not cleared yet. A cleared one is
/// removed on the way (spec: "a signing mark ... remove[s] through it"). With
/// no readable profile there are no grants, so every request stays open.
pub fn open_requests(mur_home: &Path, agent: &str) -> Vec<OpenRequest> {
    let home = mur_home.join("agents").join(agent);
    let Ok((_, p)) = crate::cmd::agent::load_profile_for_edit(agent) else {
        return read_pending(&home, Utc::now())
            .into_iter()
            .map(|request| OpenRequest {
                request,
                granted: false,
            })
            .collect();
    };
    let st = status::status(mur_home, agent, &p.entitlements.ssh);
    still_open(&home, &st.pairs, st.requests)
}

pub fn still_open(
    agent_home: &Path,
    pairs: &[PairStatus],
    requests: Vec<RequestStatus>,
) -> Vec<OpenRequest> {
    let mut out = Vec::new();
    for r in requests {
        if r.cleared(pairs) {
            if let Err(e) = mur_ssh_request::remove(agent_home, &r.request) {
                tracing::debug!("ssh request cleanup: {e}");
            }
            continue;
        }
        let granted = r
            .pairs
            .iter()
            .any(|&i| !matches!(pairs[i].state, PairState::NotApproved));
        out.push(OpenRequest {
            request: r.request,
            granted,
        });
    }
    out
}

/// Every agent's open, not-yet-granted requests, for an inbox. An agent with
/// no request file is skipped before its profile or channel is read, so a
/// poll over many agents stays a directory listing each.
pub fn open_everywhere(mur_home: &Path) -> Vec<(String, OpenRequest)> {
    let now = Utc::now();
    let mut out = Vec::new();
    for agent in crate::a2a_dial::agent_dir_names(mur_home) {
        let home = mur_home.join("agents").join(&agent);
        if read_pending(&home, now).is_empty() {
            continue;
        }
        for o in open_requests(mur_home, &agent) {
            if !o.granted {
                out.push((agent.clone(), o));
            }
        }
    }
    out
}

/// Remove the request for `target`, if one is pending. `Ok(false)`: there was
/// none. Deletes only through `mur_ssh_request::remove`.
pub fn dismiss(mur_home: &Path, agent: &str, target: &RequestHost) -> std::io::Result<bool> {
    let home = mur_home.join("agents").join(agent);
    match read_pending(&home, Utc::now())
        .into_iter()
        .find(|r| &r.target == target)
    {
        Some(r) => mur_ssh_request::remove(&home, &r).map(|()| true),
        None => Ok(false),
    }
}

/// What resumes the agent after a card opened for its own request.
pub fn hand_back_text(r: &PendingRequest) -> String {
    format!(
        "SSH access to {} is set up (you asked: \"{}\"). Run one test connection and \
         report the result.",
        r.target.canonical(),
        r.reason
    )
}

/// What resumes the agent after a setup the user started (Hub "Add host").
pub fn direct_hand_back_text(t: &RequestHost) -> String {
    format!(
        "SSH access to {} is set up. Run one test connection and report the result.",
        t.canonical()
    )
}
