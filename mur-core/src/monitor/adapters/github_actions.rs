//! `source.type: github_actions` — one GET per check against
//! `/repos/{owner}/{repo}/actions/runs/{run_id}` (spec §MVP Adapter →
//! GitHub Actions). Read-only: rerun and log download are plan-2 actions.
//! `classify` is pure over (status, body) so every fixture in the spec's
//! adapter contract tests runs without a network.

use std::str::FromStr;
use std::time::Duration;

use mur_common::secret::SecretRef;
use mur_monitor::adapter::{Observation, SourceAdapter};
use mur_monitor::spec::SourceType;
use mur_monitor::state::Outcome;

const DEFAULT_API_BASE: &str = "https://api.github.com";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Used when GitHub rate-limits without a `Retry-After` header.
pub const RATE_LIMIT_DEFAULT_SECS: u64 = 60;
/// Body bytes kept as evidence — enough to read an error, never a log dump.
pub const EVIDENCE_MAX_CHARS: usize = 160;
const USER_AGENT: &str = concat!("mur/", env!("CARGO_PKG_VERSION"));

pub struct GithubActionsAdapter {
    pub api_base: String,
    pub timeout: Duration,
}

impl Default for GithubActionsAdapter {
    fn default() -> Self {
        Self {
            api_base: DEFAULT_API_BASE.into(),
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

pub fn parse_reference(r: &str) -> Result<(String, String, u64), String> {
    let parts: Vec<&str> = r.split('/').collect();
    let [owner, repo, run] = parts.as_slice() else {
        return Err("reference must be owner/repo/run_id".into());
    };
    if owner.is_empty() || repo.is_empty() {
        return Err("owner and repo must not be empty".into());
    }
    let run_id = run
        .parse::<u64>()
        .map_err(|_| "run_id must be a number".to_string())?;
    Ok((owner.to_string(), repo.to_string(), run_id))
}

fn snippet(body: &str) -> String {
    body.chars().take(EVIDENCE_MAX_CHARS).collect()
}

pub fn classify(status: u16, body: &str, retry_after_secs: Option<u64>) -> Observation {
    // Redact before truncating. `snippet()` cuts at a fixed character count,
    // and the redaction patterns (e.g. `ghp_[A-Za-z0-9]{36}`) are fixed-length
    // matches: if a secret straddles that cut, truncating first leaves a
    // partial fragment that no longer matches the pattern and `.redacted()`
    // at the end would let it through unredacted. Redacting the full raw
    // body up front is safe either way — truncating a redaction placeholder
    // loses nothing, whereas truncating a secret defeats the pattern.
    let body = mur_common::redact::redact_secrets(body).into_owned();
    let body = body.as_str();
    let obs = match status {
        200 => classify_ok(body),
        401 => Observation::unknown(format!("credential rejected (401): {}", snippet(body)))
            .credential_failure(),
        403 if body.to_ascii_lowercase().contains("rate limit") => {
            Observation::unknown("rate limited (403)").with_poll_after(Duration::from_secs(
                retry_after_secs.unwrap_or(RATE_LIMIT_DEFAULT_SECS),
            ))
        }
        // Not rate-limited, so this 403 means the token itself lacks scope —
        // a credential problem, not a transient one (the rate-limit arm above
        // already claimed the other 403 cause).
        403 => Observation::unknown(format!(
            "forbidden (403) — token may lack actions:read: {}",
            snippet(body)
        ))
        .credential_failure(),
        404 => Observation::unknown(
            "not found (404): eventual consistency, permissions, or deleted — not proven",
        ),
        429 => Observation::unknown("rate limited (429)").with_poll_after(Duration::from_secs(
            retry_after_secs.unwrap_or(RATE_LIMIT_DEFAULT_SECS),
        )),
        s if s >= 500 => Observation::unknown(format!("github {s}: {}", snippet(body))),
        s => Observation::unknown(format!("unexpected http {s}: {}", snippet(body))),
    };
    obs.redacted()
}

fn classify_ok(body: &str) -> Observation {
    let v: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            return Observation::unknown(format!("malformed response: {e}; {}", snippet(body)));
        }
    };
    let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
    let attempt = v.get("run_attempt").and_then(|a| a.as_u64()).unwrap_or(0);
    let updated = v.get("updated_at").and_then(|u| u.as_str()).unwrap_or("");
    match status {
        "completed" => match v.get("conclusion").and_then(|c| c.as_str()) {
            Some("success") => {
                Observation::terminal(Outcome::Succeeded, format!("success (attempt {attempt})"))
            }
            Some(c @ ("failure" | "timed_out" | "startup_failure")) => {
                Observation::terminal(Outcome::Failed, format!("{c} (attempt {attempt})"))
            }
            Some("cancelled") => {
                Observation::terminal(Outcome::Cancelled, format!("cancelled (attempt {attempt})"))
            }
            Some(other) => {
                Observation::unknown(format!("completed with unrecognised conclusion `{other}`"))
            }
            None => Observation::unknown("completed but no conclusion in the response"),
        },
        "queued" | "in_progress" | "waiting" | "requested" | "pending" => Observation::pending(
            format!("{status}:{updated}:{attempt}"),
            format!("{status} (attempt {attempt}, updated {updated})"),
        ),
        other => Observation::unknown(format!("unrecognised run status `{other}`")),
    }
}

impl GithubActionsAdapter {
    fn fetch(&self, owner: &str, repo: &str, run_id: u64, token: Option<&str>) -> Observation {
        let api_base = self.api_base.clone();
        let timeout = self.timeout;
        let owner = owner.to_string();
        let repo = repo.to_string();
        let token = token.map(str::to_string);
        // `reqwest::blocking::ClientBuilder::build` panics when dropped inside
        // a Tokio runtime context (`cmd::monitor::add` runs on the CLI's
        // `block_on`) — documented behaviour, and the exact hazard this repo
        // already defends against in `dispatch.rs`, `model_prices.rs`, and
        // `model_discovery.rs::discover_models_for`. Mirror that fix: run the
        // whole request on a dedicated OS thread with no ambient runtime, so
        // the caller's context never matters.
        std::thread::spawn(move || -> Observation {
            let client = match reqwest::blocking::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(timeout)
                .build()
            {
                Ok(c) => c,
                Err(e) => return Observation::unknown(format!("http client: {e}")),
            };
            let url = format!("{api_base}/repos/{owner}/{repo}/actions/runs/{run_id}");
            let mut req = client
                .get(url)
                .header("Accept", "application/vnd.github+json");
            if let Some(t) = &token {
                req = req.bearer_auth(t);
            }
            match req.send() {
                Err(e) => Observation::unknown(format!("request failed: {e}")),
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let retry_after = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|h| h.to_str().ok())
                        .and_then(|s| s.parse::<u64>().ok());
                    let body = resp.text().unwrap_or_default();
                    classify(status, &body, retry_after)
                }
            }
        })
        .join()
        .unwrap_or_else(|_| Observation::unknown("github fetch worker thread panicked"))
    }
}

impl SourceAdapter for GithubActionsAdapter {
    fn source_type(&self) -> SourceType {
        SourceType::GithubActions
    }

    fn validate_reference(&self, reference: &str) -> Result<(), String> {
        parse_reference(reference).map(|_| ())
    }

    fn observe(&self, reference: &str, credential_ref: Option<&str>) -> Observation {
        let (owner, repo, run_id) = match parse_reference(reference) {
            Ok(p) => p,
            Err(e) => return Observation::unknown(e),
        };
        // A configured credential that cannot be resolved pauses the query
        // (spec §錯誤處理: credential 失效) — we do not fall back to
        // unauthenticated and quietly get a different answer.
        let token = match credential_ref {
            None => None,
            Some(c) => match SecretRef::from_str(c)
                .ok()
                .and_then(|r| r.resolve_to_string_blocking())
            {
                Some(t) => Some(t),
                None => {
                    return Observation::unknown(format!(
                        "credential_ref `{c}` could not be resolved — update the reference"
                    ))
                    .credential_failure()
                    .redacted();
                }
            },
        };
        self.fetch(&owner, &repo, run_id, token.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_json(status: &str, conclusion: Option<&str>, updated: &str, attempt: u32) -> String {
        serde_json::json!({
            "status": status,
            "conclusion": conclusion,
            "updated_at": updated,
            "run_attempt": attempt,
        })
        .to_string()
    }

    #[test]
    fn reference_is_owner_repo_run_id() {
        assert_eq!(
            parse_reference("mur-run/mur/123").unwrap(),
            ("mur-run".into(), "mur".into(), 123)
        );
        assert!(parse_reference("mur-run/mur").is_err());
        assert!(parse_reference("mur-run/mur/abc").is_err());
        assert!(parse_reference("a/b/1/extra").is_err());
    }

    #[test]
    fn completed_conclusions_map_to_terminal() {
        assert_eq!(
            classify(200, &run_json("completed", Some("success"), "t", 1), None).outcome,
            Outcome::Succeeded
        );
        assert_eq!(
            classify(200, &run_json("completed", Some("failure"), "t", 1), None).outcome,
            Outcome::Failed
        );
        assert_eq!(
            classify(200, &run_json("completed", Some("timed_out"), "t", 1), None).outcome,
            Outcome::Failed
        );
        assert_eq!(
            classify(200, &run_json("completed", Some("cancelled"), "t", 1), None).outcome,
            Outcome::Cancelled
        );
        assert_eq!(
            classify(200, &run_json("completed", Some("mystery"), "t", 1), None).outcome,
            Outcome::Unknown
        );
        assert_eq!(
            classify(200, &run_json("completed", None, "t", 1), None).outcome,
            Outcome::Unknown
        );
    }

    #[test]
    fn in_progress_is_pending_and_updated_at_or_attempt_is_progress() {
        let a = classify(
            200,
            &run_json("in_progress", None, "2026-09-15T12:00:00Z", 1),
            None,
        );
        let b = classify(
            200,
            &run_json("in_progress", None, "2026-09-15T12:05:00Z", 1),
            None,
        );
        let c = classify(
            200,
            &run_json("queued", None, "2026-09-15T12:05:00Z", 2),
            None,
        );
        assert_eq!(a.outcome, Outcome::Pending);
        assert_ne!(a.progress_token, b.progress_token);
        assert_ne!(b.progress_token, c.progress_token);
    }

    #[test]
    fn every_non_answer_is_unknown_never_failed() {
        for (status, body) in [
            (404, "{\"message\":\"Not Found\"}"),
            (401, "{\"message\":\"Bad credentials\"}"),
            (403, "{\"message\":\"API rate limit exceeded\"}"),
            (429, ""),
            (500, ""),
            (502, "<html>"),
            (200, "not json"),
            (200, "{\"status\":\"completed\"}"),
        ] {
            let o = classify(status, body, None);
            assert_eq!(o.outcome, Outcome::Unknown, "http {status}: {body}");
            assert!(o.adapter_error.is_some(), "http {status}");
        }
        assert!(
            classify(401, "", None)
                .adapter_error
                .unwrap()
                .contains("credential")
        );
        assert!(
            classify(404, "", None)
                .adapter_error
                .unwrap()
                .contains("not proven")
        );
    }

    /// `credential_failure` is the structural signal `cmd::monitor::add`
    /// refuses on — it must be set on the two response shapes that genuinely
    /// mean the credential is the problem (401, and a 403 that is not a rate
    /// limit) and never on an `Unknown` caused by something else, so a
    /// transient/unrelated failure never blocks monitor creation.
    #[test]
    fn credential_failure_flags_401_and_scope_403_only() {
        assert!(classify(401, "{\"message\":\"Bad credentials\"}", None).credential_failure);
        assert!(classify(403, "Resource not accessible by integration", None).credential_failure);
        assert!(!classify(403, "API rate limit exceeded", None).credential_failure);
        assert!(!classify(404, "", None).credential_failure);
        assert!(!classify(429, "", None).credential_failure);
        assert!(!classify(500, "", None).credential_failure);
    }

    #[test]
    fn rate_limit_recommends_retry_after_or_the_default() {
        assert_eq!(
            classify(429, "", Some(120)).recommended_poll_after,
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            classify(403, "API rate limit exceeded", None).recommended_poll_after,
            Some(Duration::from_secs(RATE_LIMIT_DEFAULT_SECS))
        );
        assert_eq!(
            classify(403, "Resource not accessible by integration", None).recommended_poll_after,
            None
        );
    }

    #[test]
    fn evidence_is_truncated_and_redacted() {
        let body = format!("{{\"message\":\"token ghp_{} rejected\"}}", "A".repeat(36));
        let o = classify(401, &body, None);
        assert!(
            o.evidence.len() <= EVIDENCE_MAX_CHARS + 32,
            "{}",
            o.evidence
        );
        assert!(!o.evidence.contains(&"A".repeat(36)), "{}", o.evidence);
    }

    /// A secret positioned to straddle the `EVIDENCE_MAX_CHARS` truncation
    /// boundary must never leave a recognisable fragment in evidence. Prior
    /// to the redact-before-truncate fix, `classify` truncated the raw body
    /// first and only redacted the already-cut string; a 36-char GitHub PAT
    /// cut at char 20 no longer matches the fixed-length redaction pattern,
    /// so the leading `ghp_` plus a run of the token's characters survived
    /// untouched. Redacting the full body before truncating closes that gap.
    #[test]
    fn secret_straddling_truncation_boundary_is_fully_redacted() {
        // Word boundaries (space) on both sides so the `ghp_...` pattern's
        // `\b` anchors match once the secret is whole.
        let prefix = "z ".repeat(70); // 140 chars, ends in a space
        let secret = format!("ghp_{}", "A".repeat(36)); // 40 chars
        let body = format!("{prefix}{secret} trailing text after the secret, well past the cut");
        assert!(
            body.len() > EVIDENCE_MAX_CHARS,
            "fixture must be long enough to truncate"
        );

        let o = classify(401, &body, None);

        assert!(!o.evidence.contains("ghp_"), "{}", o.evidence);
        assert!(!o.evidence.contains(&"A".repeat(10)), "{}", o.evidence);
    }

    /// An unresolvable configured `credential_ref` must pause the query at
    /// resolution and never fall back to an unauthenticated request — the
    /// difference between "this private repo is unreachable, tell someone"
    /// and "this private repo reads 404 forever and the monitor lies
    /// quietly". `env:` with a variable that is guaranteed unset resolves to
    /// `None` deterministically, with no network or keychain involved, so
    /// this exercises `observe()` without any I/O double.
    #[test]
    fn unresolvable_credential_ref_pauses_and_does_not_fall_back_unauthenticated() {
        const MISSING_VAR: &str = "MUR_TEST_GITHUB_ACTIONS_CRED_DEFINITELY_NOT_SET_XJ9K";
        assert!(
            std::env::var_os(MISSING_VAR).is_none(),
            "fixture var must not be set in this environment"
        );

        let adapter = GithubActionsAdapter::default();
        let credential_ref = format!("env:{MISSING_VAR}");

        let started = std::time::Instant::now();
        let o = adapter.observe("mur-run/mur/123", Some(&credential_ref));
        let elapsed = started.elapsed();

        assert_eq!(o.outcome, Outcome::Unknown);
        assert!(
            o.credential_failure,
            "an unresolvable credential_ref must set credential_failure so `add` refuses"
        );
        let err = o
            .adapter_error
            .expect("unresolved credential must report an error");
        assert!(
            err.contains(MISSING_VAR),
            "error should name the unresolved credential reference, got: {err}"
        );
        assert!(
            !err.to_ascii_lowercase().contains("request")
                && !err.to_ascii_lowercase().contains("http"),
            "error must show it stopped at credential resolution, not an HTTP call: {err}"
        );
        // No network call should have happened — this must return long
        // before the adapter's own 30s HTTP timeout could ever fire.
        assert!(
            elapsed < Duration::from_secs(5),
            "observe() took {elapsed:?}; an unresolvable credential must short-circuit before any network call"
        );
    }

    /// The regression this exists for: nothing else in this file ever drives
    /// `observe()` through to `fetch()`'s HTTP layer — every other test
    /// above exercises the pure `classify` function or the credential
    /// short-circuit, which returns before `fetch` builds a client. Against
    /// the pre-fix code (`reqwest::blocking::Client::builder().build()`
    /// called directly on the calling thread), this test — run inside a
    /// Tokio runtime via `#[tokio::test]`, the same context `cmd::monitor
    /// ::add` runs in via the CLI's `block_on` — panics with "Cannot drop a
    /// runtime in a context where blocking is not allowed" the moment the
    /// blocking client is built/dropped, which unwinds this test as a
    /// failure (panic in an async test surfaces as a failed task) rather
    /// than returning any `Observation` at all. Binding a `TcpListener` to
    /// port 0 and dropping it hands back a port nothing is listening on, so
    /// the connect itself fails fast with no network and no fixture server.
    #[tokio::test]
    async fn observe_runs_through_the_http_layer_without_panicking_in_an_async_context() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let port = listener.local_addr().unwrap().port();
        drop(listener); // certainly closed: nothing is listening on it now

        let adapter = GithubActionsAdapter {
            api_base: format!("http://127.0.0.1:{port}"),
            timeout: Duration::from_secs(2),
        };
        let obs = adapter.observe("owner/repo/1", None);

        assert_eq!(obs.outcome, Outcome::Unknown, "{obs:?}");
        let err = obs
            .adapter_error
            .expect("a failed connection must report an adapter_error");
        assert!(
            err.contains("request failed"),
            "expected the request-failure message, got: {err}"
        );
    }
}
