//! AgentResolver — spec §混合處置策略 step 3.
//!
//! Consulted only when the structured rules could not settle a terminal
//! failure: the spec's `on_failure` list was empty, or every action in it
//! failed. It proposes ONE remedy, which then travels the ordinary action
//! path — claimed, risk-classified, gated — so nothing here decides whether
//! something is safe to run.
//!
//! Three properties are load-bearing, in this order:
//!
//! 1. **The verb set is closed.** A proposal may only name a verb this
//!    module lists. `risk::classify` would map an unknown verb to
//!    `Privileged`, which is safe but useless (no executor), and it would
//!    put a model's invented string into the monitor's action list. So an
//!    unrecognised `action_type` voids the WHOLE response rather than being
//!    downgraded — the model gets no partial credit for a malformed answer.
//! 2. **The tier still comes from the table.** This module never returns a
//!    `RiskTier`, never reads one from the model, and never accepts one in
//!    the JSON. `RiskTier`'s own doc says tiers are never LLM-asserted;
//!    honouring that means the proposal carries a verb and nothing else that
//!    could influence gating.
//! 3. **Failure is silence, not a guess.** Every rejection path returns
//!    `Err` and the caller records an event; none of them fall back to
//!    "notify the user instead" or to a default action. A resolver that
//!    invents an action when it could not understand the model is worse than
//!    one that does nothing.

use mur_monitor::spec::Action;

/// The verbs a proposal may name.
///
/// Deliberately NOT derived from `mur_monitor::action::risk::classify`'s
/// match: that function classifies *any* string (unknown => `Privileged`)
/// because it must never fail open, whereas this is the much smaller set the
/// resolver is permitted to *propose*. They are different questions, and
/// wiring one to the other would silently widen this set the next time a
/// verb is added to the risk table.
///
/// `reschedule_monitor` is absent on purpose: it has no executor (returning
/// a settled monitor to `sleeping` un-freezes its fence and re-runs the
/// whole action list every poll), so proposing it would park an action that
/// can never run.
pub const PROPOSABLE: &[&str] = &["notify", "collect_logs", "rerun"];

/// A parsed, validated proposal. Construction is the validation: there is no
/// way to build one holding a verb outside [`PROPOSABLE`].
///
/// No `PartialEq`: `mur_monitor::spec::Action` has none, and adding a derive
/// to another crate to make assertions terser is not worth the coupling —
/// the tests compare the two fields they care about.
#[derive(Debug, Clone)]
pub struct Proposal {
    /// Ready to hand to the ordinary claim path.
    pub action: Action,
    /// Why, in the model's words, for the monitor event a human will read.
    /// Required — an action in an audit trail with no stated reason is worse
    /// than no action.
    pub reason: String,
}

/// Why a response was refused. Each variant is a fact about the response,
/// phrased for a monitor event: the operator needs to know the resolver was
/// consulted and produced nothing usable, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalError {
    /// Not JSON at all, after the one fenced-block allowance below.
    NotJson,
    /// JSON, but not an object.
    NotAnObject,
    /// No `action_type`, or it was not a string.
    MissingActionType,
    /// A verb outside `PROPOSABLE`. Carries it verbatim so the event can say
    /// what was asked for — redacted by the caller like any other model text.
    UnknownActionType(String),
    /// No `reason`, or it was blank.
    MissingReason,
    /// `params` was present but not an object.
    ParamsNotAnObject,
}

impl std::fmt::Display for ProposalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotJson => write!(f, "response was not JSON"),
            Self::NotAnObject => write!(f, "response JSON was not an object"),
            Self::MissingActionType => write!(f, "no string `action_type`"),
            Self::UnknownActionType(t) => {
                write!(f, "`action_type` is not a proposable verb: {t}")
            }
            Self::MissingReason => write!(f, "no non-blank `reason`"),
            Self::ParamsNotAnObject => write!(f, "`params` was not an object"),
        }
    }
}

/// Strip one ```-fenced block, if the whole response is exactly that.
///
/// The single leniency in this module, and it is about transport rather than
/// content: models routinely wrap JSON in a fence, and rejecting that would
/// make the resolver fail constantly for a reason no operator can act on.
/// Anything looser — scanning for the first `{`, or repairing trailing
/// commas — would start accepting responses nobody wrote deliberately, so
/// this does not do it.
fn unfence(raw: &str) -> &str {
    let t = raw.trim();
    let Some(rest) = t.strip_prefix("```") else {
        return t;
    };
    // ```json\n{…}\n```  — drop the language tag line and the closing fence.
    let Some(body) = rest.split_once('\n').map(|(_tag, b)| b) else {
        return t;
    };
    match body.trim_end().strip_suffix("```") {
        Some(inner) => inner.trim(),
        None => t,
    }
}

/// Parse a model response into a proposal, refusing anything it is not sure
/// about. See the module doc for why every failure is total.
pub fn parse_proposal(raw: &str) -> Result<Proposal, ProposalError> {
    let value: serde_json::Value =
        serde_json::from_str(unfence(raw)).map_err(|_| ProposalError::NotJson)?;
    let obj = value.as_object().ok_or(ProposalError::NotAnObject)?;

    let action_type = obj
        .get("action_type")
        .and_then(|v| v.as_str())
        .ok_or(ProposalError::MissingActionType)?;
    if !PROPOSABLE.contains(&action_type) {
        return Err(ProposalError::UnknownActionType(action_type.to_string()));
    }

    let reason = obj
        .get("reason")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .ok_or(ProposalError::MissingReason)?
        .to_string();

    let params = match obj.get("params") {
        None | Some(serde_json::Value::Null) => serde_json::Map::new(),
        Some(serde_json::Value::Object(m)) => m.clone(),
        Some(_) => return Err(ProposalError::ParamsNotAnObject),
    };

    Ok(Proposal {
        action: Action {
            r#type: action_type.to_string(),
            params,
        },
        reason,
    })
}

/// Why a consultation produced nothing. Both arms end the same way — an
/// event, no action — but they are kept apart because they mean different
/// things to an operator: one is "the model was not reachable", the other is
/// "the model answered and the answer was unusable".
#[derive(Debug, Clone)]
pub enum ResolverError {
    /// Could not reach or complete the call. Note what this is NOT: a work
    /// failure or a monitor failure. The resolver is advisory, so a failed
    /// consultation leaves the monitor exactly where it was — the same
    /// reasoning that makes an unreadable source `unknown` rather than
    /// `failed`.
    Unreachable(String),
    /// The model answered; [`parse_proposal`] refused it.
    Refused(ProposalError),
}

impl std::fmt::Display for ResolverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(e) => write!(f, "resolver not reachable: {e}"),
            Self::Refused(e) => write!(f, "resolver reply refused: {e}"),
        }
    }
}

/// Ask once, and accept only an answer [`parse_proposal`] approves.
///
/// Generic over the client rather than taking `&dyn LlmClient`, because
/// `LlmClient::complete` returns `impl Future` and the trait is therefore
/// not dyn-compatible. That is also what makes this testable without a
/// network: the tests below pass a mock.
///
/// One call, no retry. A retry loop here would multiply both the cost and
/// the disclosure surface of a feature whose whole budget is one proposal
/// per cycle, and a model that answered unusably once is not obviously
/// likelier to answer usably the second time.
pub async fn ask<C: mur_common::llm::LlmClient>(
    client: &C,
    ctx: &prompt::Context,
) -> Result<Proposal, ResolverError> {
    let raw = client
        .complete(&prompt::user_prompt(ctx), Some(&prompt::system_prompt()))
        .await
        .map_err(|e| ResolverError::Unreachable(e.to_string()))?;
    parse_proposal(&raw).map_err(ResolverError::Refused)
}

pub mod prompt;

#[cfg(test)]
mod tests;
