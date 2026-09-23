//! Frozen contract for slice 2: the `Step` schema written to
//! `runs/<name>/actions.yaml` (SPEC §3.2).
//!
//! Slice 1 ships the types, serde round-trip, and the **schema validation**
//! that decides whether a step may be written at all. The interception logic
//! (turning a `browser_click` into a `Step`) is slice 2's job and lives in
//! `RecordHook` (not here yet).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

use crate::{
    locator::{SnapshotNode, candidates_for_ref, parse_snapshot},
    proxy::{Decision, Downstream, Hook, Request},
};

/// What a recorded step does. Mirrors the Playwright MCP tool it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Goto,
    Click,
    Fill,
    Select,
    Press,
    Hover,
    AssertVisible,
    AssertText,
    AssertValue,
}

impl Action {
    /// Whether this action addresses an element (needs `locators`).
    pub fn needs_locator(self) -> bool {
        !matches!(self, Action::Goto)
    }

    /// Whether replay should fail the run when this step misses
    /// (assertions only matter in `mode: test`).
    pub fn is_assert(self) -> bool {
        matches!(
            self,
            Action::AssertVisible | Action::AssertText | Action::AssertValue
        )
    }

    /// The Playwright MCP tool name replay must send for this action.
    ///
    /// This is the **reverse** of `RecordHook::action_for`'s match: where
    /// several tool names record as the same `Action` (`browser_type` /
    /// `browser_fill` both become `Fill`), `tool_name` picks the one
    /// `action_for` lists first, so `tool_name(a).pipe(action_for) == a`
    /// round-trips (see `tests::tool_name_round_trips_through_action_for`).
    pub fn tool_name(self) -> &'static str {
        match self {
            Action::Goto => "browser_navigate",
            Action::Click => "browser_click",
            Action::Fill => "browser_type",
            Action::Select => "browser_select_option",
            Action::Press => "browser_press_key",
            Action::Hover => "browser_hover",
            Action::AssertVisible => "browser_verify_element_visible",
            Action::AssertText => "browser_verify_text_visible",
            Action::AssertValue => "browser_verify_value",
        }
    }
}

/// The `arguments` key that carries a step's `value`, keyed by [`Action`].
///
/// Single source of truth for both directions: `RecordHook::value_for` reads
/// it out of a live MCP request, [`call_for`] writes it back in for replay.
/// `None` means this action has no scalar value argument (`Click`, `Hover`,
/// `AssertVisible`).
fn value_key(action: Action) -> Option<&'static str> {
    match action {
        Action::Goto => Some("url"),
        Action::Fill => Some("text"),
        Action::Select => Some("values"),
        Action::Press => Some("key"),
        Action::AssertText => Some("text"),
        // @playwright/mcp 0.0.82 `browser_verify_value` takes `value`.
        Action::AssertValue => Some("value"),
        Action::Click | Action::Hover | Action::AssertVisible => None,
    }
}

/// Reverse of recording: turn a saved [`Step`] into the MCP `tools/call`
/// `(name, arguments)` pair replay sends downstream.
///
/// `arguments` carries the step's `value` under [`value_key`] (when the
/// action has one) and, for any action where [`Action::needs_locator`] is
/// true, an `element` key holding the step's best-priority locator string.
/// Replay resolves that locator against a fresh snapshot itself — this
/// function never touches [`Step::ref_at_record`], which is diagnostic-only.
///
/// Errors if a locator-needing step has no locators (a `Step` that passed
/// [`validate`] should always have at least one, so this indicates the
/// caller built the `Step` by hand rather than through recording).
pub fn call_for(step: &Step) -> anyhow::Result<(String, Value)> {
    let mut args = serde_json::Map::new();

    if let Some(value) = &step.value
        && let Some(key) = value_key(step.action)
    {
        args.insert(key.to_string(), Value::String(value.clone()));
    }

    if step.action.needs_locator() {
        let locator = step.locators.first().ok_or_else(|| {
            anyhow::anyhow!(
                "step {} ({:?}) needs a locator but has none",
                step.step,
                step.action
            )
        })?;
        args.insert("element".to_string(), Value::String(locator.clone()));
    }

    Ok((step.action.tool_name().to_string(), Value::Object(args)))
}

/// One line of `actions.yaml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Step {
    /// 1-based position in the run.
    pub step: u32,
    /// Human intent, ≥ 4 chars. Auto-filled from the snapshot when the agent
    /// didn't call `mur_intent` (then `intent_auto` is true).
    pub intent: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub intent_auto: bool,
    pub action: Action,
    /// URL for `goto`, text for `fill`/`press`/`select`, expected text for
    /// asserts. Secrets are stored as `{{secret:<site>/<KEY>}}` — never plain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Priority-ordered locator candidates (see [`crate::locator`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locators: Vec<String>,
    /// Set by replay `--heal` when a new locator was prepended.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub healed: bool,
    /// Index into `locators` that hit on the last replay.
    #[serde(default)]
    pub last_hit: usize,
    /// The `@ref` at record time. Debug only — replay must never use it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_at_record: Option<String>,
}

/// Whole file: header + steps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Run {
    pub name: String,
    /// `test` or `automation`.
    pub mode: Mode,
    /// Site profile used (`profiles/<site>/`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub recorded_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Test,
    Automation,
}

impl std::str::FromStr for Mode {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "test" => Ok(Mode::Test),
            "automation" => Ok(Mode::Automation),
            other => anyhow::bail!("mode must be test|automation, got {other:?}"),
        }
    }
}

/// Why a step was refused. The message is what the agent sees in the
/// JSON-RPC error, so it says what to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    NoLocator,
    /// `assert_visible` without a `role:` locator. @playwright/mcp 0.0.82's
    /// `browser_verify_element_visible` takes `{ role, accessibleName }`, so
    /// replay cannot verify a step that only has testid/label/text locators.
    NoRoleLocator,
    IntentTooShort,
    RawSecret,
}

/// Placeholder shape produced by `mur_secret`.
pub const SECRET_PLACEHOLDER_PREFIX: &str = "{{secret:";

pub fn is_secret_placeholder(s: &str) -> bool {
    s.starts_with(SECRET_PLACEHOLDER_PREFIX) && s.ends_with("}}")
}

/// Heuristic from SPEC §3.2: len ≥ 12 and has lower + upper + digit.
pub fn looks_high_entropy(s: &str) -> bool {
    s.chars().count() >= 12
        && s.chars().any(|c| c.is_ascii_lowercase())
        && s.chars().any(|c| c.is_ascii_uppercase())
        && s.chars().any(|c| c.is_ascii_digit())
}

/// Validate one step before it may be written. `in_password_field` comes
/// from the snapshot cache (slice 2) — `true` when the target element is a
/// `textbox` with `type=password`.
///
/// Unstable locators are **pruned** here (via [`crate::locator::is_stable`]);
/// the caller gets back the cleaned step or a [`Reject`].
pub fn validate(mut step: Step, in_password_field: bool) -> Result<Step, Reject> {
    if step.action.needs_locator() {
        step.locators.retain(|l| crate::locator::is_stable(l));
        if step.locators.is_empty() {
            return Err(Reject::NoLocator);
        }
        // Replay sends `{ role, accessibleName }` for this action, which only a
        // role locator can supply.
        if step.action == Action::AssertVisible
            && !step.locators.iter().any(|l| {
                matches!(
                    crate::locator::Locator::parse(l),
                    Ok(crate::locator::Locator::Role { .. })
                )
            })
        {
            return Err(Reject::NoRoleLocator);
        }
    }
    if step.intent.chars().count() < 4 {
        return Err(Reject::IntentTooShort);
    }
    if step.action == Action::Fill
        && in_password_field
        && let Some(v) = &step.value
        && !is_secret_placeholder(v)
        && looks_high_entropy(v)
    {
        return Err(Reject::RawSecret);
    }
    Ok(step)
}

/// Serialize a run to YAML (the on-disk form).
pub fn to_yaml(run: &Run) -> anyhow::Result<String> {
    Ok(serde_yaml::to_string(run)?)
}

/// Parse `actions.yaml`.
pub fn from_yaml(s: &str) -> anyhow::Result<Run> {
    Ok(serde_yaml::from_str(s)?)
}

/// JSON Schema for `actions.yaml` — exported by `mur browser show --schema`.
pub fn json_schema() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(Run)).unwrap_or_default()
}

/// Recording state owned by the MCP proxy for one `mur browser record` run.
///
/// It intercepts successful Playwright actions and persists only validated
/// steps.  The hook deliberately records on the response path: a failed click
/// must not become a replayable action.
#[derive(Debug)]
pub struct RecordHook {
    run: Run,
    /// `mur_intent` applies to exactly the next recorded action.
    pending_intent: Option<String>,
    actions_path: Option<PathBuf>,
    /// Most recent Playwright accessibility snapshot. It is only an in-memory
    /// recording aid: refs from it never reach `actions.yaml` as locators.
    snapshot: Vec<SnapshotNode>,
}

impl RecordHook {
    /// Start recording into an empty or pre-existing run.  This constructor is
    /// useful for tests; production uses [`Self::with_actions_path`].
    pub fn new(run: Run) -> Self {
        Self {
            run,
            pending_intent: None,
            actions_path: None,
            snapshot: Vec::new(),
        }
    }

    /// Record to `actions.yaml`, creating its parent directory on the first
    /// successful action.
    pub fn with_actions_path(run: Run, actions_path: PathBuf) -> Self {
        Self {
            run,
            pending_intent: None,
            actions_path: Some(actions_path),
            snapshot: Vec::new(),
        }
    }

    /// The run accumulated so far.
    pub fn run(&self) -> &Run {
        &self.run
    }

    fn intent_for_next_step(&mut self, fallback: &str) -> (String, bool) {
        match self.pending_intent.take() {
            Some(intent) if intent.chars().count() >= 4 => (intent, false),
            _ => (fallback.to_string(), true),
        }
    }

    fn persist(&self) -> anyhow::Result<()> {
        let Some(path) = &self.actions_path else {
            return Ok(());
        };
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("actions path has no parent: {}", path.display()))?;
        std::fs::create_dir_all(parent)?;
        std::fs::write(path, to_yaml(&self.run)?)?;
        Ok(())
    }

    fn action_for(req: &Request) -> Option<Action> {
        match req.tool_name()? {
            "browser_navigate" => Some(Action::Goto),
            "browser_click" => Some(Action::Click),
            "browser_type" | "browser_fill" => Some(Action::Fill),
            "browser_select_option" => Some(Action::Select),
            "browser_press_key" => Some(Action::Press),
            "browser_hover" => Some(Action::Hover),
            "browser_verify_element_visible" => Some(Action::AssertVisible),
            "browser_verify_text_visible" | "browser_verify_element_text" => {
                Some(Action::AssertText)
            }
            "browser_verify_value" | "browser_verify_element_value" => Some(Action::AssertValue),
            _ => None,
        }
    }

    fn value_for(action: Action, args: Option<&Value>) -> Option<String> {
        let key = value_key(action)?;
        args?
            .get(key)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    }

    fn locator_for(&self, req: &Request) -> Vec<String> {
        let Some(args) = req.tool_args() else {
            return Vec::new();
        };
        // Prefer candidates derived from the current a11y snapshot. A recorded
        // ref is diagnostic-only; it is resolved here while it is still valid.
        if let Some(reference) = args.get("ref").and_then(Value::as_str) {
            let candidates = candidates_for_ref(reference, &self.snapshot);
            if !candidates.is_empty() {
                return candidates;
            }
        }
        // `browser_verify_element_visible` (0.0.82) addresses the element by
        // `{ role, accessibleName }` instead of a ref. Reuse the snapshot
        // node's full candidate list when it matches, so testid/text
        // fallbacks survive; otherwise the role locator alone.
        if let Some(role) = args
            .get("role")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|r| !r.is_empty())
        {
            let name = args
                .get("accessibleName")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if let Some(node) = self
                .snapshot
                .iter()
                .find(|n| n.role == role && n.name == name)
            {
                let candidates = candidates_for_ref(&node.reference, &self.snapshot);
                if !candidates.is_empty() {
                    return candidates;
                }
            }
            let locator = crate::locator::Locator::Role {
                role: role.to_owned(),
                name: (!name.is_empty()).then(|| name.to_owned()),
            };
            return vec![locator.to_string_canonical()];
        }
        // A human-readable MCP element description is a conservative fallback
        // when no snapshot has been observed yet.
        args.get("element")
            .or_else(|| args.get("description"))
            .or_else(|| args.get("name"))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(|s| vec![format!("text:{s}")])
            .unwrap_or_default()
    }

    fn cache_snapshot_response(&mut self, req: &Request, resp: &Value) {
        if req.tool_name() != Some("browser_snapshot") || resp.get("error").is_some() {
            return;
        }
        let Some(content) = resp
            .get("result")
            .and_then(|result| result.get("content"))
            .and_then(Value::as_array)
        else {
            return;
        };
        let text = content
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            self.snapshot = parse_snapshot(&text);
        }
    }

    fn record_success(&mut self, req: &Request) -> std::result::Result<(), Reject> {
        let Some(action) = Self::action_for(req) else {
            return Ok(());
        };
        let fallback = match action {
            Action::Goto => "前往指定網址",
            Action::Click => "點擊目標元素",
            Action::Fill => "輸入欄位內容",
            Action::Select => "選取下拉選項",
            Action::Press => "按下鍵盤按鍵",
            Action::Hover => "滑過目標元素",
            Action::AssertVisible => "確認目標元素可見",
            Action::AssertText => "確認目標文字",
            Action::AssertValue => "確認欄位值",
        };
        let (intent, intent_auto) = self.intent_for_next_step(fallback);
        let args = req.tool_args();
        let mut step = Step {
            step: self.run.steps.len() as u32 + 1,
            intent,
            intent_auto,
            action,
            value: Self::value_for(action, args),
            locators: self.locator_for(req),
            healed: false,
            last_hit: 0,
            ref_at_record: args
                .and_then(|a| a.get("ref"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        };
        // Navigation does not need a locator; for all other actions the
        // current slice refuses raw `@ref` calls until a stable locator was
        // supplied. Slice 3 replaces this with generated candidates.
        step = validate(step, false)?;
        self.run.steps.push(step);
        // Persistence errors are represented as an invalid action response;
        // losing the recording silently would be worse.
        self.persist().map_err(|_| Reject::NoLocator)
    }
}

impl Hook for RecordHook {
    async fn on_request(&mut self, req: Request, _down: &Downstream) -> Decision {
        match req.tool_name() {
            Some("mur_intent") => {
                let text = req
                    .tool_args()
                    .and_then(|a| a.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if text.chars().count() < 4 {
                    return Decision::Reject {
                        code: -32602,
                        message: "intent too short (need ≥ 4 chars)".into(),
                    };
                }
                self.pending_intent = Some(text.to_string());
                Decision::Reply(serde_json::json!({"queued": true}))
            }
            Some("browser_storage_state") | Some("browser_set_storage_state") => Decision::Reject {
                code: -32000,
                message: "use mur browser auth".into(),
            },
            Some("browser_start_recording") | Some("browser_stop_recording") => Decision::Reject {
                code: -32000,
                message: "Playwright recording is disabled; MUR records actions safely".into(),
            },
            _ => Decision::Forward(req),
        }
    }

    async fn on_response(&mut self, req: &Request, resp: Value, _down: &Downstream) -> Value {
        // MCP tool failures are encoded as a successful JSON-RPC response with
        // `result.isError: true`. Recording one would make a run claim that an
        // action succeeded when Playwright rejected it.
        if resp.get("error").is_some()
            || resp
                .get("result")
                .and_then(|result| result.get("isError"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            return resp;
        }
        self.cache_snapshot_response(req, &resp);
        if Self::action_for(req).is_none() {
            return resp;
        }
        if let Err(error) = self.record_success(req) {
            let id = resp.get("id").cloned().unwrap_or(Value::Null);
            return serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32000, "message": error.to_string()}
            });
        }
        resp
    }

    fn extra_tools(&self) -> Vec<Value> {
        vec![serde_json::json!({
            "name": "mur_intent",
            "description": "Set the human intent for the next recorded browser action.",
            "inputSchema": {
                "type": "object",
                "properties": {"text": {"type": "string", "minLength": 4}},
                "required": ["text"]
            }
        })]
    }
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Reject::NoLocator => "step has no stable locator; call browser_snapshot then retry",
            Reject::NoRoleLocator => {
                "assert_visible needs a role locator: call browser_verify_element_visible with the element's role and accessibleName from browser_snapshot"
            }
            Reject::IntentTooShort => {
                "intent too short (need ≥ 4 chars): call mur_intent before the action"
            }
            Reject::RawSecret => {
                "value looks like a raw secret in a password field; use mur_secret to get a {{secret:…}} placeholder"
            }
        };
        f.write_str(s)
    }
}
impl std::error::Error for Reject {}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(action: Action, locators: &[&str]) -> Step {
        Step {
            step: 1,
            intent: "在搜尋框輸入 AirPods Pro".into(),
            intent_auto: false,
            action,
            value: Some("AirPods Pro".into()),
            locators: locators.iter().map(|s| s.to_string()).collect(),
            healed: false,
            last_hit: 0,
            ref_at_record: Some("@e21".into()),
        }
    }

    /// All 9 `Action` variants, for tests that must cover every one.
    const ALL_ACTIONS: [Action; 9] = [
        Action::Goto,
        Action::Click,
        Action::Fill,
        Action::Select,
        Action::Press,
        Action::Hover,
        Action::AssertVisible,
        Action::AssertText,
        Action::AssertValue,
    ];

    fn tools_call(name: &str, args: Value) -> Request {
        serde_json::from_value(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": args},
        }))
        .unwrap()
    }

    /// 0.1 — `tool_name()` must round-trip through `action_for`'s lookup: for
    /// every `Action`, wrapping its `tool_name()` in a `tools/call` request
    /// and feeding it back through `action_for` must yield the same
    /// `Action`. Where several tool names map to one `Action` (`Fill`),
    /// `tool_name()` is only required to hit the one `action_for` picks
    /// first — it does not need to be exhaustive over the whole fan-in.
    #[test]
    fn tool_name_round_trips_through_action_for() {
        for action in ALL_ACTIONS {
            let req = tools_call(action.tool_name(), serde_json::json!({}));
            assert_eq!(
                RecordHook::action_for(&req),
                Some(action),
                "tool_name({action:?}) = {:?} did not round-trip back through action_for",
                action.tool_name(),
            );
        }
    }

    /// 0.2 — `call_for`'s `arguments` must carry the step's value under the
    /// exact key `value_for` reads it back out of, for every action that has
    /// one (`Goto→url`, `Fill→text`, `Press→key`, asserts→text).
    #[test]
    fn call_for_uses_value_for_key_names() {
        let cases = [
            (Action::Goto, "url"),
            (Action::Fill, "text"),
            (Action::Press, "key"),
            (Action::AssertText, "text"),
            (Action::AssertValue, "value"),
        ];
        for (action, key) in cases {
            let s = step(action, &["role:button"]);
            let (_, args) = call_for(&s).unwrap();
            assert_eq!(
                args.get(key).and_then(Value::as_str),
                s.value.as_deref(),
                "call_for({action:?}) missing/mismatched {key:?} key: {args}"
            );
        }
    }

    /// 0.3 — locator-needing steps must carry a locator (`ref` or `element`)
    /// in `call_for`'s arguments; `Goto` must carry neither.
    #[test]
    fn call_for_carries_locator_only_when_needed() {
        for action in ALL_ACTIONS {
            let locators: &[&str] = if action.needs_locator() {
                &["role:button[name=\"Go\"]"]
            } else {
                &[]
            };
            let s = step(action, locators);
            let (_, args) = call_for(&s).unwrap();
            let has_locator = args.get("ref").is_some() || args.get("element").is_some();
            assert_eq!(
                has_locator,
                action.needs_locator(),
                "call_for({action:?}) locator presence {has_locator} != needs_locator() {}: {args}",
                action.needs_locator(),
            );
        }
    }

    #[test]
    fn assert_visible_rejects_without_role_locator() {
        let s = step(Action::AssertVisible, &["testid:checkout", "text:Checkout"]);
        assert_eq!(validate(s, false).unwrap_err(), Reject::NoRoleLocator);
    }

    #[test]
    fn assert_visible_keeps_role_and_fallbacks() {
        let s = step(
            Action::AssertVisible,
            &["role:button[name=\"Checkout\"]", "testid:checkout"],
        );
        let s = validate(s, false).unwrap();
        assert_eq!(
            s.locators,
            vec!["role:button[name=\"Checkout\"]", "testid:checkout"]
        );
    }

    #[test]
    fn other_actions_still_accept_testid_only() {
        for action in [Action::Click, Action::Hover, Action::AssertValue] {
            let s = step(action, &["testid:quantity-input"]);
            assert!(validate(s, false).is_ok(), "{action:?}");
        }
    }

    /// 0.0.82 `browser_verify_element_visible` carries `{ role, accessibleName }`
    /// and no `ref`, so the role locator has to come from those arguments.
    #[test]
    fn locator_for_verify_visible_uses_role_and_accessible_name() {
        let hook = RecordHook::new(Run {
            name: "assert".into(),
            mode: Mode::Test,
            profile: None,
            recorded_at: chrono::Utc::now(),
            steps: vec![],
        });
        let req = tools_call(
            "browser_verify_element_visible",
            serde_json::json!({"role": "button", "accessibleName": "Checkout"}),
        );
        assert_eq!(
            hook.locator_for(&req),
            vec!["role:button[name=\"Checkout\"]"]
        );
        let unnamed = tools_call(
            "browser_verify_element_visible",
            serde_json::json!({"role": "banner", "accessibleName": ""}),
        );
        assert_eq!(hook.locator_for(&unnamed), vec!["role:banner"]);
    }

    #[test]
    fn locator_for_verify_visible_keeps_snapshot_fallbacks() {
        let mut hook = RecordHook::new(Run {
            name: "assert".into(),
            mode: Mode::Test,
            profile: None,
            recorded_at: chrono::Utc::now(),
            steps: vec![],
        });
        hook.snapshot =
            crate::locator::parse_snapshot("- button \"Checkout\" [ref=e9] [data-testid=checkout]");
        let req = tools_call(
            "browser_verify_element_visible",
            serde_json::json!({"role": "button", "accessibleName": "Checkout"}),
        );
        let locators = hook.locator_for(&req);
        assert_eq!(locators[0], "role:button[name=\"Checkout\"]");
        assert!(
            locators.contains(&"testid:checkout".to_string()),
            "{locators:?}"
        );
    }

    #[test]
    fn reject_when_no_locator() {
        let s = step(Action::Click, &[]);
        assert_eq!(validate(s, false).unwrap_err(), Reject::NoLocator);
    }

    #[test]
    fn reject_when_only_unstable_locators() {
        // the `agent-browser去pchome` yaml:59 case — nothing but an @ref-ish css chain
        let s = step(Action::Click, &["css:div > div > ul > li:nth-child(3) > a"]);
        assert_eq!(validate(s, false).unwrap_err(), Reject::NoLocator);
    }

    #[test]
    fn prunes_unstable_keeps_stable() {
        let s = step(
            Action::Click,
            &[
                "css:.css-1x2y3z",
                "role:button[name=\"登入\"]",
                "testid:login",
            ],
        );
        let s = validate(s, false).unwrap();
        assert_eq!(
            s.locators,
            vec!["role:button[name=\"登入\"]", "testid:login"]
        );
    }

    #[test]
    fn reject_short_intent() {
        let mut s = step(Action::Click, &["testid:x"]);
        s.intent = "點".into();
        assert_eq!(validate(s, false).unwrap_err(), Reject::IntentTooShort);
    }

    #[test]
    fn reject_raw_secret_in_password_field() {
        let mut s = step(Action::Fill, &["role:textbox[name=\"密碼\"]"]);
        s.value = Some("Hunter2Hunter2x9".into());
        assert_eq!(validate(s.clone(), true).unwrap_err(), Reject::RawSecret);
        // same value in a non-password field is fine
        assert!(validate(s.clone(), false).is_ok());
        // placeholder is fine even in a password field
        s.value = Some("{{secret:pchome/PASSWORD}}".into());
        assert!(validate(s, true).is_ok());
    }

    #[test]
    fn goto_needs_no_locator() {
        let mut s = step(Action::Goto, &[]);
        s.value = Some("https://24h.pchome.com.tw".into());
        assert!(validate(s, false).is_ok());
    }

    #[test]
    fn yaml_round_trip_matches_spec_shape() {
        let run = Run {
            name: "smoke".into(),
            mode: Mode::Test,
            profile: Some("pchome".into()),
            recorded_at: chrono::DateTime::parse_from_rfc3339("2026-09-10T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            steps: vec![step(
                Action::Fill,
                &["role:searchbox[name=\"搜尋\"]", "testid:search-input"],
            )],
        };
        let y = to_yaml(&run).unwrap();
        assert!(y.contains("intent: 在搜尋框輸入 AirPods Pro"), "{y}");
        assert!(y.contains("action: fill"), "{y}");
        assert!(y.contains("- role:searchbox[name=\"搜尋\"]"), "{y}");
        assert!(!y.contains("healed"), "false flags are omitted: {y}");
        assert_eq!(from_yaml(&y).unwrap(), run);
    }

    #[test]
    fn schema_exports() {
        let s = json_schema();
        assert!(s["title"].as_str().is_some() || s["$schema"].as_str().is_some());
    }

    #[test]
    fn record_hook_persists_successful_navigation_as_first_step() {
        // Public seam for slice 2: the proxy hook, not recorder internals.
        // This deliberately fails until `RecordHook` exists and owns the run.
        let hook = RecordHook::new(Run {
            name: "smoke".into(),
            mode: Mode::Test,
            profile: None,
            recorded_at: chrono::Utc::now(),
            steps: vec![],
        });
        assert!(hook.run().steps.is_empty());
    }

    #[tokio::test]
    async fn proxy_does_not_record_mcp_tool_failures() {
        // Exercise the public proxy seam: MCP reports tool failures inside a
        // JSON-RPC `result` with `isError`, rather than a JSON-RPC error.
        use crate::proxy::run_io;
        use serde_json::json;
        use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, duplex};

        async fn server(mut input: impl AsyncRead + Unpin, mut output: impl AsyncWrite + Unpin) {
            let mut lines = BufReader::new(&mut input).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": request["id"].clone(),
                    "result": {
                        "isError": true,
                        "content": [{"type": "text", "text": "button not found"}]
                    }
                });
                output
                    .write_all(response.to_string().as_bytes())
                    .await
                    .unwrap();
                output.write_all(b"\n").await.unwrap();
                output.flush().await.unwrap();
            }
        }

        let dir =
            std::env::temp_dir().join(format!("mur-browser-failed-action-{}", std::process::id()));
        let actions = dir.join("actions.yaml");
        let hook = RecordHook::with_actions_path(
            Run {
                name: "failed-click".into(),
                mode: Mode::Test,
                profile: None,
                recorded_at: chrono::Utc::now(),
                steps: vec![],
            },
            actions.clone(),
        );
        let (mut agent_write, agent_input) = duplex(16 * 1024);
        let (agent_output, mut agent_read) = duplex(16 * 1024);
        let (server_input, server_read) = duplex(16 * 1024);
        let (server_write, server_output) = duplex(16 * 1024);
        tokio::spawn(server(server_read, server_write));
        tokio::spawn(run_io(
            agent_input,
            agent_output,
            server_input,
            server_output,
            hook,
        ));

        agent_write.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"browser_click\",\"arguments\":{\"element\":\"missing button\"}}}\n").await.unwrap();
        let mut lines = BufReader::new(&mut agent_read).lines();
        let response: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(response["result"]["isError"], true);
        assert!(!actions.exists(), "failed MCP calls must not be recorded");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn proxy_records_intent_and_successful_navigation_to_yaml() {
        use crate::proxy::run_io;
        use serde_json::json;
        use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, duplex};

        async fn server(mut input: impl AsyncRead + Unpin, mut output: impl AsyncWrite + Unpin) {
            let mut lines = BufReader::new(&mut input).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": request["id"].clone(),
                    "result": {"content": [{"type": "text", "text": "ok"}]}
                });
                output
                    .write_all(response.to_string().as_bytes())
                    .await
                    .unwrap();
                output.write_all(b"\n").await.unwrap();
                output.flush().await.unwrap();
            }
        }

        let suffix = format!("mur-browser-record-{}", std::process::id());
        let dir = std::env::temp_dir().join(suffix);
        let actions = dir.join("actions.yaml");
        let hook = RecordHook::with_actions_path(
            Run {
                name: "smoke".into(),
                mode: Mode::Test,
                profile: None,
                recorded_at: chrono::Utc::now(),
                steps: vec![],
            },
            actions.clone(),
        );
        let (mut agent_write, agent_input) = duplex(16 * 1024);
        let (agent_output, mut agent_read) = duplex(16 * 1024);
        let (server_input, server_read) = duplex(16 * 1024);
        let (server_write, server_output) = duplex(16 * 1024);
        tokio::spawn(server(server_read, server_write));
        tokio::spawn(run_io(
            agent_input,
            agent_output,
            server_input,
            server_output,
            hook,
        ));

        agent_write.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"mur_intent\",\"arguments\":{\"text\":\"Open the MUR homepage\"}}}\n").await.unwrap();
        let mut lines = BufReader::new(&mut agent_read).lines();
        let intent_reply: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(intent_reply["result"]["queued"], true);

        agent_write.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"browser_navigate\",\"arguments\":{\"url\":\"https://example.test\"}}}\n").await.unwrap();
        let navigation_reply: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(navigation_reply["id"], 2);

        let saved = from_yaml(&std::fs::read_to_string(&actions).unwrap()).unwrap();
        assert_eq!(saved.steps.len(), 1);
        assert_eq!(saved.steps[0].intent, "Open the MUR homepage");
        assert!(!saved.steps[0].intent_auto);
        assert_eq!(saved.steps[0].action, Action::Goto);
        assert_eq!(
            saved.steps[0].value.as_deref(),
            Some("https://example.test")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn proxy_snapshot_then_click_records_stable_locator_candidates() {
        use crate::proxy::run_io;
        use serde_json::json;
        use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, duplex};

        async fn server(mut input: impl AsyncRead + Unpin, mut output: impl AsyncWrite + Unpin) {
            let mut lines = BufReader::new(&mut input).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let request: Value = serde_json::from_str(&line).unwrap();
                let text = match request["params"]["name"].as_str() {
                    Some("browser_snapshot") => {
                        "- button \"Submit\" [ref=e4] [data-testid=submit-button]"
                    }
                    _ => "ok",
                };
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": request["id"].clone(),
                    "result": {"content": [{"type": "text", "text": text}]}
                });
                output
                    .write_all(response.to_string().as_bytes())
                    .await
                    .unwrap();
                output.write_all(b"\n").await.unwrap();
                output.flush().await.unwrap();
            }
        }

        let dir =
            std::env::temp_dir().join(format!("mur-browser-snapshot-click-{}", std::process::id()));
        let actions = dir.join("actions.yaml");
        let hook = RecordHook::with_actions_path(
            Run {
                name: "snapshot-click".into(),
                mode: Mode::Test,
                profile: None,
                recorded_at: chrono::Utc::now(),
                steps: vec![],
            },
            actions.clone(),
        );
        let (mut agent_write, agent_input) = duplex(16 * 1024);
        let (agent_output, mut agent_read) = duplex(16 * 1024);
        let (server_input, server_read) = duplex(16 * 1024);
        let (server_write, server_output) = duplex(16 * 1024);
        tokio::spawn(server(server_read, server_write));
        tokio::spawn(run_io(
            agent_input,
            agent_output,
            server_input,
            server_output,
            hook,
        ));
        let mut lines = BufReader::new(&mut agent_read).lines();

        for request in [
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_snapshot","arguments":{}}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"browser_click","arguments":{"ref":"@e4"}}}),
        ] {
            agent_write
                .write_all(request.to_string().as_bytes())
                .await
                .unwrap();
            agent_write.write_all(b"\n").await.unwrap();
            let reply: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert!(reply.get("error").is_none(), "{reply}");
        }

        let saved = from_yaml(&std::fs::read_to_string(&actions).unwrap()).unwrap();
        assert_eq!(saved.steps.len(), 1);
        assert_eq!(saved.steps[0].action, Action::Click);
        assert_eq!(saved.steps[0].ref_at_record.as_deref(), Some("@e4"));
        assert_eq!(
            saved.steps[0].locators,
            vec![
                "role:button[name=\"Submit\"]".to_string(),
                "testid:submit-button".to_string(),
                "text:Submit".to_string(),
            ]
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
