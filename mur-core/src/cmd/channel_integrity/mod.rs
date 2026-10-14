//! `mur channel verify` (channel-log-integrity spec section 8, #1770).
//!
//! A thin, read-only shell over [`mur_channel::verify::verify_channel`]. Its own
//! job is the verdict: the library reports *what it saw*, this decides whether
//! that is good enough to call `ok`.
//!
//! The rule is one-directional. Only a channel whose head verifies, whose chain
//! is intact, with every key it needed resolved and every line readable, is
//! `ok`. Anything the verifier could not establish is `unverified`, which is
//! never `ok` and never silently `failed` either: "I cannot tell" and "I found
//! damage" are different facts and scripts need to tell them apart.

mod keys;
mod render;

#[cfg(test)]
mod tests;

use std::io::Write;
use std::path::Path;

use anyhow::Result;
use mur_channel::ChannelStore;
use mur_channel::verify::{ChannelReport, HeadFault, IntegrityState, LineClass, verify_channel};

use crate::channel_writer::ROUTER_AGENT;
use keys::RouterKeys;

/// Exit codes. Distinct so a script can tell damage from "could not check".
pub const EXIT_OK: i32 = 0;
pub const EXIT_FAILED: i32 = 1;
pub const EXIT_UNVERIFIED: i32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    Ok,
    /// Could not be established either way. Not `ok`.
    Unverified,
    /// Evidence of damage or tampering.
    Failed,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Unverified => "unverified",
            Self::Failed => "failed",
        }
    }
}

/// One channel's result: the library's report plus this layer's verdict.
pub struct Outcome {
    pub channel_id: String,
    pub verdict: Verdict,
    /// `None` when the channel could not even be read; see `error`.
    pub report: Option<ChannelReport>,
    /// Key versions the verifier asked for and could not get (`None` = a line
    /// with no `key_version`).
    pub unresolved_keys: Vec<Option<u32>>,
    pub error: Option<String>,
}

/// Verify `ids`, or every channel under `home` when `ids` is empty. Writes the
/// report to `out` and returns the process exit code.
pub fn run(home: &Path, ids: &[String], json: bool, out: &mut dyn Write) -> Result<i32> {
    let ids = if ids.is_empty() {
        let mut all = ChannelStore::new(home).list_ids()?;
        all.sort();
        all
    } else {
        ids.to_vec()
    };
    let outcomes = verify_all(home, &ids);
    if json {
        render::json(out, &outcomes)?;
    } else {
        render::human(out, &outcomes)?;
    }
    // The caller may `process::exit`, which does not flush.
    out.flush()?;
    Ok(exit_code(&outcomes))
}

pub fn verify_all(home: &Path, ids: &[String]) -> Vec<Outcome> {
    // The router signs heads and its own lines; one source for the whole run.
    let keys = RouterKeys::load(&home.join("agents").join(ROUTER_AGENT));
    ids.iter().map(|id| verify_one(home, id, &keys)).collect()
}

fn verify_one(home: &Path, id: &str, keys: &RouterKeys) -> Outcome {
    keys.reset_misses();
    match verify_channel(home, id, &|kv| keys.resolve(kv)) {
        Ok(report) => {
            let unresolved_keys = keys.misses();
            Outcome {
                channel_id: id.to_string(),
                verdict: judge(&report, &unresolved_keys),
                report: Some(report),
                unresolved_keys,
                error: None,
            }
        }
        // An unreadable channel is not evidence of tampering, and not ok.
        Err(e) => Outcome {
            channel_id: id.to_string(),
            verdict: Verdict::Unverified,
            report: None,
            unresolved_keys: keys.misses(),
            error: Some(format!("{e:#}")),
        },
    }
}

/// The verdict for one report. Damage beats uncertainty; `Ok` needs everything.
pub fn judge(report: &ChannelReport, unresolved_keys: &[Option<u32>]) -> Verdict {
    if report.seq_findings.iter().any(|f| !f.legacy) {
        return Verdict::Failed;
    }
    match &report.state {
        // A head naming a key we do not hold may be forged or may just be older
        // than our rotation record. We cannot tell, so we do not say.
        IntegrityState::HeadInvalid(HeadFault::UnknownKeyVersion(_)) => Verdict::Unverified,
        s if s.is_error() => Verdict::Failed,
        // Not sealed / nothing there / orphaned head: proves nothing (spec 8.3).
        IntegrityState::NoHead | IntegrityState::NoLog | IntegrityState::LogMissing => {
            Verdict::Unverified
        }
        // Intact only means unchanged since seal, and only for what we could
        // check. Unresolved keys, unreadable lines, and lines past the head
        // (not tamper-evident, spec 8.2) each leave part of the log unchecked.
        _ if !unresolved_keys.is_empty()
            || !report.unparseable_lines.is_empty()
            || report.lines.iter().any(|l| l.class == LineClass::Unsealed) =>
        {
            Verdict::Unverified
        }
        _ => Verdict::Ok,
    }
}

/// Worst verdict wins: failed (1) over unverified (2) over ok (0).
pub fn exit_code(outcomes: &[Outcome]) -> i32 {
    match outcomes.iter().map(|o| o.verdict).max() {
        Some(Verdict::Failed) => EXIT_FAILED,
        Some(Verdict::Unverified) => EXIT_UNVERIFIED,
        Some(Verdict::Ok) | None => EXIT_OK,
    }
}
