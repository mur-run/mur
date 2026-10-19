//! Ordered delivery of server responses to the agent, off the reader loop.
//!
//! `Hook::on_response` may call [`Downstream::call`]. That reply arrives on
//! the server's stdout, which only the server reader routes, so the reader
//! must never wait on a hook. Responses go through one worker instead: a
//! single consumer keeps them in the order the server sent them.

use super::{Downstream, Hook, Request, merge_extra_tools};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;

/// One server response to an agent request, with the request that caused it
/// (`None` when the id was never seen, which is passed through untouched).
pub(super) type Pending = (Option<Request>, Value);

pub(super) fn spawn_response_worker<H: Hook>(
    hook: Arc<Mutex<H>>,
    down: Downstream,
    to_agent: mpsc::UnboundedSender<String>,
) -> (mpsc::UnboundedSender<Pending>, JoinHandle<()>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Pending>();
    let handle = tokio::spawn(async move {
        while let Some((req, resp)) = rx.recv().await {
            let out = match req {
                Some(req) => {
                    let mut hook = hook.lock().await;
                    let mut resp = hook.on_response(&req, resp, &down).await;
                    if req.method == "tools/list" {
                        merge_extra_tools(&mut resp, hook.extra_tools());
                    }
                    resp
                }
                None => resp,
            };
            let _ = to_agent.send(out.to_string());
        }
    });
    (tx, handle)
}
