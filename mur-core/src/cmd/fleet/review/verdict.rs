//! §3.2 / §3.3 / §3.4: machine validation of the two structured replies a
//! review round carries — the reviewer's verdict and, once findings are
//! open, the main agent's rebuttal.
//!
//! Pure: each validator takes the current ledger and a reply and returns
//! either the payloads to sign, or a one-line reason the reply is malformed.
//! That reason is what the §3.2 "retry once with a validation hint" sends
//! back to the model (AC5), so it names the concrete problem.

use serde::Deserialize;

use super::ledger::{FoldError, Ledger};
use super::schema::{
    Cumulative, NewFindingDto, PriorUpdateDto, RebuttalAnswer, RebuttalResponseDto, ReviewPayload,
    VerdictKind,
};
use super::wire::extract_verdict_json;

/// The reviewer's wire reply (§3.2): `verdict: approve | revise | blocked`,
/// `findings:` (new findings only — the system assigns IDs, never the
/// model, per `NewFindingDto` having no `id` field at all), `prior:` (one
/// entry per previously-issued finding ID not yet closed).
#[derive(Debug, Clone, Deserialize)]
struct VerdictReply {
    verdict: VerdictKind,
    #[serde(default)]
    findings: Vec<NewFindingDto>,
    #[serde(default)]
    prior: Vec<PriorUpdateDto>,
}

/// The main agent's wire reply once findings are open (§3.4).
#[derive(Debug, Clone, Deserialize)]
struct RebuttalReply {
    responses: Vec<RebuttalResponseDto>,
}

/// A verdict that validated: the kind, the ledger with the round folded in,
/// and the payloads to sign in channel order.
#[derive(Debug, Clone)]
pub struct StagedVerdict {
    pub kind: VerdictKind,
    pub ledger: Ledger,
    pub payloads: Vec<ReviewPayload>,
}

/// Zero cumulative (§4 requires every turn-ending event to carry one; real
/// execution-time/cost accounting is not wired yet).
pub fn zero_cumulative() -> Cumulative {
    Cumulative {
        exec_time_ms: 0,
        cost_usd_micros: 0,
    }
}

/// Open finding IDs (§3.3: `open` ∪ `disputed`) that `answered` does not name.
fn missing_open_ids<'a>(
    ledger: &Ledger,
    answered: impl Iterator<Item = &'a str> + Clone,
) -> Vec<String> {
    ledger
        .open_set()
        .into_keys()
        .filter(|id| !answered.clone().any(|a| a == id))
        .collect()
}

/// §3.2 / §3.3: validate the reviewer's reply against `ledger`.
///
/// Malformed = no JSON block, a JSON block of the wrong shape, a `prior`
/// list missing a status for any finding still open (§3.3: "A missing
/// status counts as a malformed verdict"), or a status for an ID never
/// issued (§8.2 illegal transition).
pub fn parse_verdict(ledger: &Ledger, round: u32, reply: &str) -> Result<StagedVerdict, String> {
    let json = extract_verdict_json(reply).ok_or("no JSON verdict block found")?;
    let parsed: VerdictReply = serde_json::from_str(json)
        .map_err(|e| format!("the verdict JSON does not match the required shape: {e}"))?;
    let missing = missing_open_ids(ledger, parsed.prior.iter().map(|p| p.id.as_str()));
    if !missing.is_empty() {
        return Err(format!(
            "`prior` has no status for open finding(s): {}",
            missing.join(", ")
        ));
    }
    let mut scratch = ledger.clone();
    let payloads = stage_round(&mut scratch, round, &parsed).map_err(|e| e.to_string())?;
    Ok(StagedVerdict {
        kind: parsed.verdict,
        ledger: scratch,
        payloads,
    })
}

/// §3.4: validate the main agent's rebuttal against `ledger`.
///
/// Malformed = no JSON block, a JSON block of the wrong shape, an open
/// finding left unanswered, a `reject`/`partial` without a reason, or an ID
/// never issued.
pub fn parse_rebuttal(ledger: &Ledger, round: u32, reply: &str) -> Result<ReviewPayload, String> {
    let json = extract_verdict_json(reply).ok_or("no JSON responses block found")?;
    let parsed: RebuttalReply = serde_json::from_str(json)
        .map_err(|e| format!("the responses JSON does not match the required shape: {e}"))?;
    for r in &parsed.responses {
        let needs_reason = matches!(r.answer, RebuttalAnswer::Reject | RebuttalAnswer::Partial);
        let has_reason = r.reason.as_deref().is_some_and(|s| !s.trim().is_empty());
        if needs_reason && !has_reason {
            return Err(format!(
                "finding {}: a reason is required for reject and partial",
                r.id
            ));
        }
    }
    let missing = missing_open_ids(ledger, parsed.responses.iter().map(|r| r.id.as_str()));
    if !missing.is_empty() {
        return Err(format!(
            "no answer for open finding(s): {}",
            missing.join(", ")
        ));
    }
    let payload = ReviewPayload::Rebuttal {
        round,
        responses: parsed.responses,
        cumulative: zero_cumulative(),
    };
    ledger.clone().apply(&payload).map_err(|e| e.to_string())?;
    Ok(payload)
}

/// Fold one reviewer reply into `scratch`, returning the payloads in channel
/// order (§3.3.1: `verdict` last), or the first illegal transition.
fn stage_round(
    scratch: &mut Ledger,
    round: u32,
    parsed: &VerdictReply,
) -> Result<Vec<ReviewPayload>, FoldError> {
    // §3.3.1: findings first, `verdict` LAST — it seals the round on replay.
    let mut out = Vec::new();
    for f in &parsed.findings {
        let payload = ReviewPayload::FindingIssued {
            round,
            id: scratch.next_finding_id(),
            severity: f.severity,
            issue: f.issue.clone(),
        };
        scratch.apply(&payload)?;
        out.push(payload);
    }
    for p in &parsed.prior {
        let payload = ReviewPayload::FindingStatus {
            round,
            id: p.id.clone(),
            status: p.status,
            reason: p.reason.clone(),
        };
        scratch.apply(&payload)?;
        out.push(payload);
    }
    let verdict = ReviewPayload::Verdict {
        round,
        kind: parsed.verdict,
        cumulative: zero_cumulative(),
    };
    scratch.apply(&verdict)?;
    out.push(verdict);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ledger with F1 (open) issued in round 1.
    fn one_open() -> Ledger {
        parse_verdict(
            &Ledger::default(),
            1,
            r#"{"verdict":"revise","findings":[{"severity":"low","issue":"x"}]}"#,
        )
        .unwrap()
        .ledger
    }

    #[test]
    fn verdict_without_json_is_malformed() {
        let err = parse_verdict(&Ledger::default(), 1, "looks fine").unwrap_err();
        assert!(err.contains("no JSON"), "{err}");
    }

    #[test]
    fn verdict_missing_a_prior_status_is_malformed() {
        let err = parse_verdict(&one_open(), 2, r#"{"verdict":"approve"}"#).unwrap_err();
        assert!(err.contains("F1"), "{err}");
    }

    #[test]
    fn verdict_for_unissued_id_is_malformed() {
        let reply = r#"{"verdict":"approve","prior":[{"id":"F1","status":"resolved"},{"id":"F9","status":"resolved"}]}"#;
        let err = parse_verdict(&one_open(), 2, reply).unwrap_err();
        assert!(err.contains("F9"), "{err}");
    }

    #[test]
    fn rebuttal_covering_every_open_finding_is_valid() {
        let reply = "done\n```json\n{\"responses\":[{\"id\":\"F1\",\"answer\":\"accept\"}]}\n```";
        let p = parse_rebuttal(&one_open(), 2, reply).unwrap();
        assert!(matches!(p, ReviewPayload::Rebuttal { round: 2, .. }));
    }

    #[test]
    fn rebuttal_reject_without_reason_is_malformed() {
        let reply = r#"{"responses":[{"id":"F1","answer":"reject","reason":"  "}]}"#;
        let err = parse_rebuttal(&one_open(), 2, reply).unwrap_err();
        assert!(err.contains("reason"), "{err}");
    }

    #[test]
    fn rebuttal_missing_an_open_finding_is_malformed() {
        let err = parse_rebuttal(&one_open(), 2, r#"{"responses":[]}"#).unwrap_err();
        assert!(err.contains("F1"), "{err}");
    }
}
