//! Output for `mur channel verify`: human by default, `--json` for scripts.

use std::io::Write;

use anyhow::Result;
use mur_channel::verify::{
    ChannelReport, HeadFault, IntegrityState, LineClass, SeqFaultKind, SeqFinding,
};
use serde_json::{Value, json};

use super::{Outcome, Verdict};

/// Stable machine name for a state. Part of the `--json` contract.
pub fn state_name(s: &IntegrityState) -> &'static str {
    match s {
        IntegrityState::LogMissing => "log_missing",
        IntegrityState::NoLog => "no_log",
        IntegrityState::HeadInvalid(_) => "head_invalid",
        IntegrityState::NoHead => "no_head",
        IntegrityState::Gap => "gap",
        IntegrityState::Reordered => "reordered",
        IntegrityState::DuplicateSeq => "duplicate_seq",
        IntegrityState::TailTruncated => "tail_truncated",
        IntegrityState::ChainBroken { .. } => "chain_broken",
        IntegrityState::Intact => "intact",
    }
}

fn fault_name(f: &HeadFault) -> &'static str {
    match f {
        HeadFault::Unparseable(_) => "unparseable",
        HeadFault::UnknownVersion(_) => "unknown_version",
        HeadFault::WrongChannel(_) => "wrong_channel",
        HeadFault::UnknownKeyVersion(_) => "unknown_key_version",
        HeadFault::BadSignature => "bad_signature",
    }
}

fn kind_name(k: SeqFaultKind) -> &'static str {
    match k {
        SeqFaultKind::Gap => "gap",
        SeqFaultKind::Reordered => "reordered",
        SeqFaultKind::DuplicateSeq => "duplicate_seq",
    }
}

/// Lines per class: `[legacy, sealed, sealed_unsigned, unsealed]`.
fn class_counts(r: &ChannelReport) -> [usize; 4] {
    let mut c = [0usize; 4];
    for l in &r.lines {
        c[match l.class {
            LineClass::Legacy => 0,
            LineClass::Sealed => 1,
            LineClass::SealedUnsigned => 2,
            LineClass::Unsealed => 3,
        }] += 1;
    }
    c
}

fn key_label(kv: &Option<u32>) -> String {
    kv.map_or_else(|| "none".to_string(), |v| v.to_string())
}

fn finding_text(f: &SeqFinding) -> String {
    format!(
        "{} at line {} (seq {} -> {}){}",
        kind_name(f.kind),
        f.line_no,
        f.prev_seq,
        f.seq,
        if f.legacy { ", legacy" } else { "" }
    )
}

/// Why this channel got its verdict, in one clause.
fn reason(o: &Outcome) -> String {
    let Some(r) = &o.report else {
        return format!(
            "could not read: {}",
            o.error.as_deref().unwrap_or("unknown")
        );
    };
    let hard = r.seq_findings.iter().find(|f| !f.legacy);
    let mut s = match (&r.state, hard) {
        (_, Some(f)) if !matches!(r.state, IntegrityState::HeadInvalid(_)) => finding_text(f),
        (IntegrityState::HeadInvalid(HeadFault::UnknownKeyVersion(kv)), _) => format!(
            "head signed with key version {}, which is not in the router's rotation record",
            key_label(kv)
        ),
        (IntegrityState::HeadInvalid(f), _) => format!("head invalid ({})", fault_name(f)),
        (IntegrityState::ChainBroken { lines }, _) => {
            format!("the first {lines} lines no longer match the sealed chain hash")
        }
        (IntegrityState::TailTruncated, _) => "the log is shorter than the head says".into(),
        (IntegrityState::LogMissing, _) => "head exists but the log is gone".into(),
        (IntegrityState::NoLog, _) => "no log and no head".into(),
        (IntegrityState::NoHead, _) => "never sealed; proves nothing either way".into(),
        (IntegrityState::Intact, _) => "unchanged since seal".into(),
        (st, _) => state_name(st).replace('_', " "),
    };
    if !o.unresolved_keys.is_empty() {
        let v: Vec<_> = o.unresolved_keys.iter().map(key_label).collect();
        s.push_str(&format!("; no key for version {}", v.join(", ")));
    }
    let unsealed = class_counts(r)[3];
    if unsealed > 0 && !matches!(r.state, IntegrityState::NoHead) {
        s.push_str(&format!(
            "; {unsealed} line(s) past the head are not covered"
        ));
    }
    if !r.unparseable_lines.is_empty() {
        s.push_str(&format!(
            "; {} unreadable line(s)",
            r.unparseable_lines.len()
        ));
    }
    s
}

pub fn human(out: &mut dyn Write, outcomes: &[Outcome]) -> Result<()> {
    if outcomes.is_empty() {
        writeln!(out, "No channels to verify.")?;
        return Ok(());
    }
    let width = outcomes
        .iter()
        .map(|o| o.channel_id.len())
        .max()
        .unwrap_or(0);
    for o in outcomes {
        let mark = match o.verdict {
            Verdict::Ok => "ok",
            Verdict::Unverified => "UNVERIFIED",
            Verdict::Failed => "FAILED",
        };
        writeln!(out, "{:<width$}  {:<10}  {}", o.channel_id, mark, reason(o))?;
        if let Some(r) = &o.report {
            let [legacy, sealed, unsigned, unsealed] = class_counts(r);
            if legacy + sealed + unsigned + unsealed > 0 {
                writeln!(
                    out,
                    "{:<width$}              lines: {sealed} sealed, {unsigned} sealed-unsigned, \
                     {unsealed} unsealed, {legacy} legacy",
                    ""
                )?;
            }
            for f in r.seq_findings.iter().filter(|f| !f.legacy).skip(1) {
                writeln!(out, "{:<width$}              also: {}", "", finding_text(f))?;
            }
        }
    }
    let count = |v| outcomes.iter().filter(|o| o.verdict == v).count();
    writeln!(
        out,
        "\n{} channel(s): {} ok, {} unverified, {} failed. \
         `ok` means unchanged since sealed, not genuine.",
        outcomes.len(),
        count(Verdict::Ok),
        count(Verdict::Unverified),
        count(Verdict::Failed),
    )?;
    Ok(())
}

pub fn json(out: &mut dyn Write, outcomes: &[Outcome]) -> Result<()> {
    let channels: Vec<Value> = outcomes.iter().map(channel_json).collect();
    let worst = outcomes
        .iter()
        .map(|o| o.verdict)
        .max()
        .unwrap_or(Verdict::Ok);
    let doc = json!({
        "verdict": worst.as_str(),
        "exit_code": super::exit_code(outcomes),
        "channels": channels,
    });
    writeln!(out, "{}", serde_json::to_string_pretty(&doc)?)?;
    Ok(())
}

fn channel_json(o: &Outcome) -> Value {
    let unresolved: Vec<Value> = o.unresolved_keys.iter().map(|k| json!(k)).collect();
    let Some(r) = &o.report else {
        return json!({
            "channel_id": o.channel_id,
            "verdict": o.verdict.as_str(),
            "state": null,
            "error": o.error,
            "unresolved_key_versions": unresolved,
        });
    };
    let [legacy, sealed, sealed_unsigned, unsealed] = class_counts(r);
    json!({
        "channel_id": o.channel_id,
        "verdict": o.verdict.as_str(),
        "state": state_name(&r.state),
        "head_fault": match &r.state { IntegrityState::HeadInvalid(f) => json!(fault_name(f)), _ => Value::Null },
        "error": null,
        "head": r.head.as_ref().map(|h| json!({
            "seq": h.seq,
            "lines": h.lines,
            "key_version": h.key_version,
            "legacy_through": h.legacy_through,
        })),
        "lines": {
            "legacy": legacy,
            "sealed": sealed,
            "sealed_unsigned": sealed_unsigned,
            "unsealed": unsealed,
        },
        "seq_findings": r.seq_findings.iter().map(|f| json!({
            "kind": kind_name(f.kind),
            "line": f.line_no,
            "prev_seq": f.prev_seq,
            "seq": f.seq,
            "legacy": f.legacy,
        })).collect::<Vec<_>>(),
        "unparseable_lines": r.unparseable_lines,
        "unterminated_tail_bytes": r.unterminated_tail_bytes,
        "unresolved_key_versions": unresolved,
    })
}
