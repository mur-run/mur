//! Built-in `browser_request` tool (browser setup journey, Q1, #2360).
//!
//! The one route an agent has to a website it cannot use yet. It states the
//! site, the reason and whether login is needed — never a profile. It writes
//! one request file under `<agent_home>/browser-requests/` that murmur and
//! Hub turn into a card; nothing is opened, bound or logged in until the
//! user acts there. Always writes (plan D3): whether a profile is already
//! bound is the card's question, answered with `plan_bind` outside the
//! runtime, which stays browser-agnostic.

use std::path::PathBuf;

use mur_browser_request::{BrowserRequest, Site};

use crate::tools::{ToolDef, ToolError, ToolExecutor, ToolOutput};

pub const BROWSER_REQUEST: &str = "browser_request";

/// What the agent is told about the route, in the prompt.
const ROUTE: &str = "To use a website that needs the user's login, or that your browser tools \
cannot reach yet, call `browser_request` with the site, a one-line reason and whether login is \
needed, then stop and tell the user you asked. Never name a profile: the user picks one on the \
card MUR shows them. Do not ask the user to run commands (`mur browser auth`, `/browser live`, \
`mur agent perm ...`), and do not try to read, copy or export cookies or browser profiles yourself.";

/// The browser section of the system prompt; appended only when the tool is registered.
pub fn prompt_section() -> String {
    format!("\n\n## Browser\n{ROUTE}")
}

pub struct BrowserRequestTool {
    pub agent_home: PathBuf,
}

#[async_trait::async_trait]
impl ToolExecutor for BrowserRequestTool {
    fn name(&self) -> &str {
        BROWSER_REQUEST
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: BROWSER_REQUEST.into(),
            description: "Ask the user for a website in your browser: the site, why, and \
whether you need their login. The only route to a site that needs a login or is not reachable \
yet: never tell the user to run commands. Never name a profile; the user picks one. Records one \
request that MUR shows the user; stop after calling it and say you asked. Your first visit after \
they finish is the test — report what you saw."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "site": {"type": "string", "description": "URL or host name, e.g. https://git.example.com"},
                    "reason": {"type": "string", "description": "One line: why you need it"},
                    "login": {"type": "boolean", "description": "Whether the site needs the user's login"}
                },
                "required": ["site", "reason", "login"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let site = input
            .get("site")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidInput("`site` is required".into()))?;
        let site = Site::parse(site).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let login = input
            .get("login")
            .and_then(|v| v.as_bool())
            .ok_or_else(|| ToolError::InvalidInput("`login` is required: true or false".into()))?;
        let reason = input.get("reason").and_then(|v| v.as_str()).unwrap_or("");
        let req = BrowserRequest::new(&site, login, reason, chrono::Utc::now())
            .map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let written = mur_browser_request::write(&self.agent_home, &req)
            .map_err(|e| ToolError::Execution(format!("record the browser request: {e}")))?;
        Ok(format!(
            "Request recorded for {}{}. MUR lists it for the user in murmur and Hub; nothing is \
             opened, bound or logged in until they act on it there. Stop now and tell the user \
             which site you asked for and why. Do not ask them to run commands. Once they are \
             done, your first visit is the test: report what you saw, and do not call it verified.",
            written.site.origin(),
            if written.replaced {
                " (it replaces your earlier request for the same site)"
            } else {
                ""
            },
        )
        .into())
    }
}

#[cfg(test)]
#[path = "browser_request_tests.rs"]
mod tests;
