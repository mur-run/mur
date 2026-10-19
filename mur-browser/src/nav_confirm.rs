//! Confirm a "failed" navigation with a snapshot instead of trusting the error.
//!
//! Right after a navigation fails, Chromium commits its own error page
//! (`chrome-error://chromewebdata/`). If the agent navigates again before that
//! commit lands, Playwright reports the new navigation as
//! `interrupted by another navigation to "chrome-error://chromewebdata/"` even
//! though the page goes on to load. An agent that believes the error retries
//! or gives up on a page it already has.
//!
//! For that one error, this hook asks the server for `browser_snapshot` and,
//! when the tab really is on the requested origin, answers with the snapshot.
//! Anything else keeps the original error: the hook only removes a false
//! failure, it never invents a success.

use crate::proxy::{Decision, Downstream, Hook, Request};
use serde_json::{Value, json};

/// The Playwright error tail this hook re-checks.
pub const ERROR_PAGE_RACE: &str =
    "is interrupted by another navigation to \"chrome-error://chromewebdata/\"";
/// Line prefix of the page URL in a `@playwright/mcp` snapshot response.
pub const PAGE_URL_PREFIX: &str = "- Page URL: ";
/// Note put ahead of the snapshot, so the agent knows why it got one.
pub const CONFIRMED_NOTE: &str = "Navigation confirmed by browser_snapshot: Playwright reported \
     it interrupted by Chromium's error page, but the tab is on the requested page.";

/// Wraps another hook; repairs the error-page race before the inner hook
/// (recorder, broker redaction) sees the response.
pub struct NavConfirmHook<H> {
    inner: H,
}

impl<H> NavConfirmHook<H> {
    pub fn new(inner: H) -> Self {
        Self { inner }
    }
}

impl<H: Hook> Hook for NavConfirmHook<H> {
    async fn on_request(&mut self, req: Request, down: &Downstream) -> Decision {
        self.inner.on_request(req, down).await
    }

    async fn on_response(&mut self, req: &Request, resp: Value, down: &Downstream) -> Value {
        let resp = confirm(req, resp, down).await;
        self.inner.on_response(req, resp, down).await
    }

    fn extra_tools(&self) -> Vec<Value> {
        self.inner.extra_tools()
    }
}

async fn confirm(req: &Request, resp: Value, down: &Downstream) -> Value {
    if req.tool_name() != Some("browser_navigate") || !is_error_page_race(&resp) {
        return resp;
    }
    let Some(target) = req
        .tool_args()
        .and_then(|a| a.get("url"))
        .and_then(Value::as_str)
    else {
        return resp;
    };
    let snapshot = match down.call_tool("browser_snapshot", json!({})).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            tracing::warn!(%error, "navigation confirm: snapshot call failed");
            return resp;
        }
    };
    match confirmed_result(target, &snapshot) {
        Some(result) => {
            tracing::info!(
                target,
                "navigation confirmed by snapshot after error-page race"
            );
            json!({"jsonrpc": "2.0", "id": resp.get("id").cloned().unwrap_or(Value::Null), "result": result})
        }
        None => resp,
    }
}

/// True for a tool error carrying the error-page race message.
pub fn is_error_page_race(resp: &Value) -> bool {
    let Some(result) = resp.get("result") else {
        return false;
    };
    if !result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return false;
    }
    texts(result).any(|t| t.contains(ERROR_PAGE_RACE))
}

/// The result to hand back when `snapshot` shows the tab on `target`'s origin;
/// `None` when it does not, or the snapshot itself failed.
pub fn confirmed_result(target: &str, snapshot: &Value) -> Option<Value> {
    let result = snapshot.get("result")?;
    if result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let page = texts(result).find_map(|t| {
        t.lines()
            .find_map(|l| l.trim_start().strip_prefix(PAGE_URL_PREFIX))
            .map(str::trim)
    })?;
    if !same_origin(target, page) {
        return None;
    }
    let mut content = vec![json!({"type": "text", "text": CONFIRMED_NOTE})];
    content.extend(result.get("content")?.as_array()?.iter().cloned());
    Some(json!({"content": content}))
}

/// Same origin (scheme, host, port). A same-origin redirect counts; a hop to
/// another origin stays an error, since the snapshot cannot tell it apart
/// from a page the agent did not ask for.
fn same_origin(target: &str, page: &str) -> bool {
    let (Ok(t), Ok(p)) = (url::Url::parse(target), url::Url::parse(page)) else {
        return false;
    };
    let origin = t.origin();
    origin.is_tuple() && origin == p.origin()
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
