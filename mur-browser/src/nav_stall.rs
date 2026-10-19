//! Explain a `browser_navigate` that timed out, so the agent stops waiting.
//!
//! When the main document cannot be decoded (for example a response labelled
//! `Content-Encoding: gzip` whose body is not gzip), Chromium fails it with
//! `ERR_CONTENT_DECODING_FAILED` but never commits an error page: the frame
//! has no document and `domcontentloaded` never fires. Playwright then waits
//! out the whole navigation timeout, and a following `browser_snapshot` hangs
//! on the same empty frame. This reproduces with a bare
//! `chrome-headless-shell --dump-dom`, so it is not something MUR can fix.
//!
//! What this hook does: on a navigate timeout it asks the server for
//! `browser_tabs` (which only reads the tab header and returns at once) and
//! puts a plain explanation ahead of the original error. The response stays
//! an error; the hook only makes it legible.

use crate::nav_confirm::same_origin;
use crate::proxy::{Decision, Downstream, Hook, Request};
use serde_json::{Value, json};
use std::time::Duration;

/// Fragments of Playwright's navigation timeout error, e.g.
/// `Timeout 20000ms exceeded.\nCall log:\n  - navigating to "…", waiting until "domcontentloaded"`.
pub const TIMEOUT_MARKERS: [&str; 2] = ["ms exceeded", "navigating to "];
/// Bound on the internal `browser_tabs` call. It does not wait on the page,
/// so anything slower means the server itself is stuck.
pub const TABS_CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// Marker of the current tab in `browser_tabs` output.
const CURRENT_TAB: &str = "(current)";

/// Note when the tab is on the requested origin: the response arrived but
/// Chromium never produced a document.
pub const STALLED_NOTE: &str = "browser_navigate timed out, but the tab is on the requested \
     page: the server answered and Chromium never rendered it (seen when a response cannot be \
     decoded, e.g. a wrong Content-Encoding header). browser_snapshot and other page tools will \
     hang on this tab too. Do not retry this URL; navigate somewhere else or report the page as \
     unreadable.";
/// Note when the tab never left its previous page: the site did not answer
/// in time, or is unreachable.
pub const NOT_COMMITTED_NOTE: &str = "browser_navigate timed out before the requested page \
     loaded; the tab is still on another page. The site is slow or unreachable. Retry at most \
     once, or report it as unavailable.";

/// Wraps another hook; annotates navigate timeouts before the inner hook
/// (recorder, broker redaction) sees the response.
pub struct NavStallHook<H> {
    inner: H,
}

impl<H> NavStallHook<H> {
    pub fn new(inner: H) -> Self {
        Self { inner }
    }
}

impl<H: Hook> Hook for NavStallHook<H> {
    async fn on_request(&mut self, req: Request, down: &Downstream) -> Decision {
        self.inner.on_request(req, down).await
    }

    async fn on_response(&mut self, req: &Request, resp: Value, down: &Downstream) -> Value {
        let resp = explain(req, resp, down).await;
        self.inner.on_response(req, resp, down).await
    }

    fn extra_tools(&self) -> Vec<Value> {
        self.inner.extra_tools()
    }
}

async fn explain(req: &Request, resp: Value, down: &Downstream) -> Value {
    if req.tool_name() != Some("browser_navigate") || !is_navigation_timeout(&resp) {
        return resp;
    }
    let Some(target) = req
        .tool_args()
        .and_then(|a| a.get("url"))
        .and_then(Value::as_str)
    else {
        return resp;
    };
    let tabs = tokio::time::timeout(
        TABS_CALL_TIMEOUT,
        down.call_tool("browser_tabs", json!({"action": "list"})),
    )
    .await;
    let tabs = match tabs {
        Ok(Ok(tabs)) => tabs,
        Ok(Err(error)) => {
            tracing::warn!(%error, "navigation stall: browser_tabs call failed");
            return resp;
        }
        Err(_) => {
            tracing::warn!("navigation stall: browser_tabs call timed out");
            return resp;
        }
    };
    match annotated(target, &resp, &tabs) {
        Some(out) => {
            tracing::info!(target, "navigation timeout annotated from browser_tabs");
            out
        }
        None => resp,
    }
}

/// True for a tool error carrying Playwright's navigation timeout.
pub fn is_navigation_timeout(resp: &Value) -> bool {
    let Some(result) = resp.get("result") else {
        return false;
    };
    if !is_error(result) {
        return false;
    }
    texts(result).any(|t| TIMEOUT_MARKERS.iter().all(|m| t.contains(m)))
}

/// URL of the current tab in a `browser_tabs` response, from a line like
/// `- 0: (current) [Title](https://host/path)`.
pub fn current_tab_url(tabs: &Value) -> Option<&str> {
    let result = tabs.get("result")?;
    if is_error(result) {
        return None;
    }
    texts(result).find_map(|t| {
        t.lines()
            .find(|l| l.contains(CURRENT_TAB))
            .and_then(|l| l.trim_end().strip_suffix(')'))
            .and_then(|l| l.rsplit_once("]("))
            .map(|(_, url)| url)
    })
}

/// `resp` with the matching note put first; `None` when `tabs` does not say
/// where the current tab is, so the original error goes through untouched.
pub fn annotated(target: &str, resp: &Value, tabs: &Value) -> Option<Value> {
    let page = current_tab_url(tabs)?;
    let note = if same_origin(target, page) {
        STALLED_NOTE
    } else {
        NOT_COMMITTED_NOTE
    };
    let mut out = resp.clone();
    let content = out.get_mut("result")?.get_mut("content")?.as_array_mut()?;
    content.insert(0, json!({"type": "text", "text": note}));
    Some(out)
}

fn is_error(result: &Value) -> bool {
    result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn texts(result: &Value) -> impl Iterator<Item = &str> {
    result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|b| b.get("text").and_then(Value::as_str))
}

#[cfg(test)]
mod tests;
