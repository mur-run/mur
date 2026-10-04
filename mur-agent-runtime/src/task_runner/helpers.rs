use super::*;

/// Attach the settlement to a turn's reply, when the turn earned one.
///
/// Two parts on one message: the rendered card for whoever is reading, and the
/// ledger as `MessagePart::Data` for whoever is parsing. A headless caller —
/// `mur agent send`, a fleet step, the Hub — gets the same accounting as the
/// TUI without scraping prose out of the text part.
///
/// Turns that only answered a question get neither: a settlement under a
/// one-line reply is noise, and noise is how a useful signal stops being read.
/// MIME of the per-turn ledger Data part on every reply. No client renders
/// it today (grepped 2026-09-19); `remember_turn` reads it back, and the Hub
/// gets a free per-turn record when it wants one.
pub(super) const TURN_LEDGER_MIME: &str = "application/vnd.mur.turn-ledger+json";

pub(super) fn settle(text: String, ledger: &crate::turn_ledger::TurnLedger) -> Message {
    // The gate is evaluated here because this is the one place the reply
    // text and the ledger meet (spec 2026-09-19-unverified-claim-card §4).
    let mut ledger = ledger.clone();
    ledger.claims_external_state = crate::turn_ledger::claims_external_state(&text);
    let ledger = &ledger;
    let mut parts = vec![mur_common::a2a::MessagePart::Text {
        text: if ledger.warrants_settlement() {
            format!("{text}{}", crate::turn_ledger::render(ledger))
        } else {
            text
        },
    }];
    // Attached on every turn, not only when the card is shown: an empty
    // ledger is the fact memory needs most (spec §4.2).
    if let Ok(data) = serde_json::to_value(ledger) {
        parts.push(mur_common::a2a::MessagePart::Data {
            mime_type: TURN_LEDGER_MIME.into(),
            data,
        });
    }
    Message {
        role: "agent".into(),
        parts,
    }
}

/// Backoff delay for rate-limit retry attempt `attempt` (1-indexed: the first
/// retry is attempt 1). Doubles `RATE_LIMIT_BACKOFF_BASE` per attempt, giving
/// 2s, 4s, 8s for attempts 1, 2, 3 with the current base of 1s.
pub(super) fn rate_limit_backoff_delay(attempt: u8) -> std::time::Duration {
    RATE_LIMIT_BACKOFF_BASE * (1u32 << u32::from(attempt))
}

/// Deterministic hash of a tool call's arguments. Serializes to canonical JSON
/// (sorted keys via `serde_json::Value`'s BTreeMap-backed object) before hashing
/// so logically-identical args always fingerprint the same.
/// Fields a tool takes for NARRATION, not for the work. Excluded from the
/// doom-loop fingerprint below.
///
/// `description` is the whole reason the guard never fired in production. The
/// bash schema asks for "what you are doing and why, 5-10 words" and the tool
/// never reads it — but the fingerprint hashed the entire input object, so a
/// model that narrates each attempt (they do; one numbered them "1 of 6",
/// "2 of 6", …) minted a fresh `args` hash every call. `repeats` stayed at 1
/// forever and the guard could not fire for `bash`, the tool most likely to be
/// looped on. Instrumented against a live agent 2026-09-14: six `echo` calls,
/// one identical `content` hash, six distinct `args` hashes.
///
/// `remember` does read its `description`, and still loses nothing that
/// matters: `name`, `content` and `kind` stay in the fingerprint, so what
/// defines that action is intact — and three calls that agree on all of those
/// AND return identical results are a loop whatever the narration says.
///
/// The list itself lives in `mur_common::hitl::pin` because the HITL pin
/// strips the same fields for the same reason (a re-worded `description` must
/// not mint a new pin and re-ask a settled approval). One list, or the two
/// drift apart.
use mur_common::hitl::pin::NARRATION_FIELDS;

/// Hash the part of a tool's input that determines what it DOES.
///
/// Only top-level narration keys are dropped; everything else, including
/// nested objects, is hashed as-is. Non-object inputs hash whole.
pub(super) fn fingerprint_args(args: &serde_json::Value) -> u64 {
    let Some(map) = args.as_object() else {
        return fingerprint_str(&args.to_string());
    };
    if !NARRATION_FIELDS.iter().any(|k| map.contains_key(*k)) {
        return fingerprint_str(&args.to_string());
    }
    // `serde_json::Map` preserves insertion order unless the `preserve_order`
    // feature is off (then it is a BTreeMap and sorted) — either way the same
    // input yields the same string within a build, which is all the window
    // comparison needs.
    let stripped: serde_json::Map<String, serde_json::Value> = map
        .iter()
        .filter(|(k, _)| !NARRATION_FIELDS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    fingerprint_str(&serde_json::Value::Object(stripped).to_string())
}

/// Deterministic hash of an arbitrary string (used for canonical tool-result
/// content in the doom-loop fingerprint). `DefaultHasher` is fixed-seed, so the
/// same input always yields the same value within a build — sufficient for
/// equality-within-window comparison, no randomness.
pub(super) fn fingerprint_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Return a copy of `history` in which every `ToolUse` call is guaranteed to be
/// followed by a matching `tool_result`. For any `tool_use` id not covered by
/// the message immediately after its `ToolUse` turn, a synthetic
/// `ToolResults` entry is inserted right after that turn with content
/// `"[stopped: <reason>]"`. This keeps the resulting `LlmRequest` well-formed so
/// the Anthropic API does not reject it with "tool_use ids without tool_result".
pub(super) fn sanitize_dangling_tool_uses(
    history: &[crate::llm::RichMessage],
    reason: LoopStop,
) -> Vec<crate::llm::RichMessage> {
    use crate::llm::{RichMessage, ToolResultEntry};

    let mut out: Vec<RichMessage> = Vec::with_capacity(history.len() + 1);
    for (i, msg) in history.iter().enumerate() {
        out.push(msg.clone());
        if let RichMessage::ToolUse { calls, .. } = msg {
            // Ids the very next message already answers (the API requires the
            // tool_result block to come immediately after the tool_use turn).
            let covered: HashSet<&str> = match history.get(i + 1) {
                Some(RichMessage::ToolResults { results }) => {
                    results.iter().map(|r| r.call_id.as_str()).collect()
                }
                _ => HashSet::new(),
            };
            let missing: Vec<ToolResultEntry> = calls
                .iter()
                .filter(|c| !covered.contains(c.call_id.as_str()))
                .map(|c| ToolResultEntry {
                    call_id: c.call_id.clone(),
                    content: format!("[stopped: {}]", reason.as_str()),
                    is_error: true,
                    status: crate::tools::ToolStatus::Ok,
                    images: Vec::new(),
                })
                .collect();
            if !missing.is_empty() {
                out.push(RichMessage::ToolResults { results: missing });
            }
        }
    }
    out
}

/// Best-effort recovery of the most recent assistant-authored text from the
/// loop history (the inline reasoning attached to a tool-use turn).
/// Read `path` from disk, return `ArtifactInfo` with SHA-256 hash and size
/// when the file exists and is readable. `None` on any error (absent/missing
/// permissions/empty) — the task falls back to the inline reply without
/// swallowing errors (callers still get the full LLM text).
pub(super) fn detect_artifact(
    path: &std::path::Path,
) -> Option<Vec<mur_common::a2a::ArtifactInfo>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 {
        return None;
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::with_capacity(meta.len().min(256 * 1024) as usize);
    file.read_to_end(&mut buf).ok()?;
    use sha2::Digest;
    let hash = hex::encode(sha2::Sha256::digest(&buf));
    Some(vec![mur_common::a2a::ArtifactInfo {
        path: path.to_string_lossy().into_owned(),
        mime_type: guess_mime_type(path).unwrap_or_else(|| "application/octet-stream".to_string()),
        sha256: Some(hash),
        size_bytes: meta.len(),
    }])
}

/// Simple extension-based MIME guess. Not exhaustive — the artifact metadata
/// is advisory and callers who need precise MIME should probe the content.
pub(super) fn guess_mime_type(path: &std::path::Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(
        match ext.as_str() {
            "md" | "markdown" => "text/markdown",
            "txt" => "text/plain",
            "json" => "application/json",
            "yaml" | "yml" => "application/x-yaml",
            "html" | "htm" => "text/html",
            "csv" => "text/csv",
            "toml" => "application/toml",
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "svg" => "image/svg+xml",
            "pdf" => "application/pdf",
            "rs" => "text/x-rust",
            "ts" | "tsx" => "text/typescript",
            _ => "application/octet-stream",
        }
        .into(),
    )
}

pub(super) fn last_assistant_text(history: &[crate::llm::RichMessage]) -> Option<String> {
    use crate::llm::RichMessage;
    history.iter().rev().find_map(|m| match m {
        RichMessage::ToolUse { text: Some(t), .. } if !t.is_empty() => Some(t.clone()),
        RichMessage::Text { role, content } if role == "agent" || role == "assistant" => {
            Some(content.clone())
        }
        _ => None,
    })
}

/// Build a `TaskError` for a failed task outcome.
/// Operator-facing text for a refused tool call.
///
/// A bare `"denied"` reason — what the CLI sends when the user just presses `n`
/// — adds nothing the prefix has not already said, and appending it anyway
/// printed `tool call denied: denied` (#940). Only a reason that carries new
/// information is appended.
/// The answer the gate already knows, before it sends a prompt to anyone.
///
/// `Some(false)` means the caller told us it cannot answer an approval prompt —
/// a one-shot `mur agent send`, a cron fire, a script. Waiting on it burns the
/// whole `hitl.timeout_secs` and then returns this same denial, and the silence
/// in between reads as "the agent had nothing to say". That cost a
/// misdiagnosis, not just five minutes: the turn came back with no agent
/// message and nothing naming approval as the cause.
///
/// `Some(true)` and `None` both fall through to asking — an interactive client,
/// or no routed entry at all, where the existing no-sink handling applies.
///
/// Extracted because the same shape as an inline condition was untestable: a
/// test of the map that carries the flag says nothing about whether the gate
/// reads it.
/// Does this result take its tool off the table for the rest of the turn?
/// A gate's refusal does (policy Deny, `ToolError::NotAuthorized`); a name the
/// model invented does not — there is nothing to withdraw.
/// Does this result take the tool off the table for the rest of the turn?
///
/// Only a `Tool`-scoped denial does — a policy denial or an authorization
/// refusal, which is what CLAUDE.md documents ("any authorization refusal
/// (`not authorized:`) withdraws that tool for the rest of the turn"). An
/// `Action`-scoped denial refused one path or one binary; the tool still
/// works for everything else, and withdrawing it there cost a whole job on
/// 2026-09-13. See [`crate::tools::DenialScope`].
pub(super) fn withdraws(entry: &crate::llm::ToolResultEntry) -> bool {
    matches!(
        entry.status,
        crate::tools::ToolStatus::Denied {
            scope: crate::tools::DenialScope::Tool,
            ..
        }
    )
}

pub(crate) fn decide_without_asking(
    can_approve: Option<bool>,
    tool_name: &str,
) -> Option<crate::hitl::HitlDecision> {
    if can_approve != Some(false) {
        return None;
    }
    Some(crate::hitl::HitlDecision {
        allow: false,
        reason: Some(format!(
            "`{tool_name}` needs approval and this caller cannot give one — run it from \
             `murmur`, or allow the tool with `mur agent perm tool-allow <agent> {tool_name}`"
        )),
        surface: None,
    })
}

/// Byte cap on the arguments logged for a call refused without asking.
pub(crate) const DENIED_ARGS_LOG_MAX_BYTES: usize = 512;

/// Redacted, capped view of a refused call's arguments for `stderr.log`.
///
/// A non-interactive denial ends the task before any `step/*` event or
/// conversation record is written, so this log line is the only trace of
/// *what* the model asked for (#1685: which path did `read_file` want?).
/// Secrets are redacted and the length is capped so a huge or sensitive
/// argument cannot flood or leak through the log.
pub(crate) fn denied_args_preview(input: &serde_json::Value) -> String {
    let raw = input.to_string();
    let red = mur_common::redact::redact_secrets(&raw);
    if red.len() <= DENIED_ARGS_LOG_MAX_BYTES {
        return red.into_owned();
    }
    let mut end = DENIED_ARGS_LOG_MAX_BYTES;
    while !red.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… ({} bytes total)", &red[..end], red.len())
}

pub(crate) fn deny_message(reason: Option<&str>) -> String {
    match reason.map(str::trim) {
        Some(r) if !r.is_empty() && r != "denied" => format!("tool call denied: {r}"),
        _ => "tool call denied".to_string(),
    }
}

pub(crate) fn task_error(code: &str, message: String, recoverable: bool) -> TaskError {
    TaskError {
        code: code.to_string(),
        message,
        recoverable,
        details: None,
    }
}

pub(super) fn text_of(m: &Message) -> String {
    m.parts
        .iter()
        .find_map(|p| match p {
            MessagePart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The base64 payload of an `image/*` Data part, if that is what `p` is — the
/// one predicate for "this input carries an image", shared by `user_message`
/// (which renders it) and `remember_turn` (which only counts it).
pub(super) fn image_part(p: &MessagePart) -> Option<(String, String)> {
    match p {
        MessagePart::Data { mime_type, data } if mime_type.starts_with("image/") => data
            .get("base64")
            .and_then(|v| v.as_str())
            .map(|b64| (mime_type.clone(), b64.to_string())),
        _ => None,
    }
}

/// How many images the user attached to `input`.
pub(super) fn image_count(input: &Message) -> u32 {
    input
        .parts
        .iter()
        .filter(|p| image_part(p).is_some())
        .count() as u32
}

/// The turn ledger `settle` attached to `reply`, if present and readable.
pub(super) fn ledger_of(reply: &Message) -> Option<crate::turn_ledger::TurnLedger> {
    reply.parts.iter().find_map(|p| match p {
        MessagePart::Data { mime_type, data } if mime_type == TURN_LEDGER_MIME => {
            serde_json::from_value(data.clone()).ok()
        }
        _ => None,
    })
}

/// Build the user-turn message: image+text when `input` carries a pasted
/// image (a screenshot from `mur agent cli`), else plain text. Images skip the
/// B0 text hook (they're binary, not prompt-injectable). ponytail: OCR-scan
/// inbound images later if needed.
pub(super) fn user_message(input: &Message) -> crate::llm::RichMessage {
    use crate::llm::RichMessage;
    let text = text_of(input);
    let image = input.parts.iter().find_map(image_part);
    match image {
        Some((media_type, data)) => RichMessage::ImageText {
            role: input.role.clone(),
            media_type,
            data,
            text,
        },
        None => RichMessage::Text {
            role: input.role.clone(),
            content: text,
        },
    }
}

/// Effective policy for one tool call.
///
/// Extracted from the gate so the ordering is testable: an inline
/// `if A || (B && C)` compiled fine with the `&& C` removed, which would have
/// let an exemption silently override an operator's explicit `deny`.
///
/// Order is the whole content:
/// 1. `suggest_replies` is a no-op the model uses to offer choices.
/// 2. An explicit rule always wins — exemptions set defaults, never overrides.
/// 3. `recall` defaults to Allow: a pure read of this agent's own snapshot,
///    which cannot surface anything the injector would not have injected given
///    more budget. Under `Ask` it parks for `hitl.timeout_secs` on every path
///    with no human to answer.
/// 4. Everything else falls to `ToolPolicy::default()` — `Ask`, fail-closed.
///    Dispatch/spend tools (`parallel_jobs`, `fleet_run`, `delegate_to`) must
///    ask BEFORE executing, which is what makes `Ask` real spend protection.
/// 5. #1600: an `Allow` from steps 2–3 becomes `Ask` when a matching rule
///    declares `risk:` above `Write` — `allow` means "policy does not refuse",
///    not "skip the risk gate". `Deny` is never loosened.
pub(crate) fn effective_tool_policy(
    rules: &[mur_common::agent::ToolRule],
    tool_name: &str,
) -> mur_common::agent::ToolPolicy {
    use mur_common::agent::{ToolPolicy, resolve_tool_policy_opt};
    if crate::tools::suggest::suggest_replies_allowed(tool_name) {
        return ToolPolicy::Allow;
    }
    let base = match resolve_tool_policy_opt(rules, tool_name)
        .or_else(|| resolve_tool_policy_opt(rules, policy_name(tool_name)))
    {
        Some(explicit) => explicit,
        None if crate::tools::recall::recall_needs_no_approval(tool_name) => ToolPolicy::Allow,
        None => ToolPolicy::default(),
    };
    if base == ToolPolicy::Allow
        && declared_tool_risk(rules, tool_name)
            .is_some_and(|tier| !mur_common::hitl::tier_may_be_granted(tier))
    {
        return ToolPolicy::Ask;
    }
    base
}

/// The strictest `risk:` declared for `tool_name`, looked up under the same
/// two names the policy uses (D11: the control tools also answer as `bash`).
/// Shipped on the approval request so the CLI gates on the same tier.
pub(crate) fn declared_tool_risk(
    rules: &[mur_common::agent::ToolRule],
    tool_name: &str,
) -> Option<mur_common::hitl::RiskTier> {
    use mur_common::agent::resolve_tool_risk;
    resolve_tool_risk(rules, tool_name).max(resolve_tool_risk(rules, policy_name(tool_name)))
}

/// D11: the control tools resolve as themselves first, then as `bash`.
pub(super) fn policy_name(tool: &str) -> &str {
    match tool {
        crate::tools::bash_control::BASH_WAIT | crate::tools::bash_control::BASH_KILL => "bash",
        other => other,
    }
}

pub(super) fn strip_lines_for(text: &str, names: &HashSet<&str>) -> String {
    if names.is_empty() {
        return text.to_string();
    }
    text.lines()
        .filter(|line| {
            !names
                .iter()
                .any(|n| line.contains(&format!("[Skill: {n} ")))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Maximum byte size of tool output included inline in a `step/completed`
/// notification. Larger outputs are truncated; full recovery in a later phase.
pub(crate) const STEP_MAX_BYTES: usize = 8 * 1024;

/// Wrap params in a JSON-RPC notification envelope — mirrors the existing
/// `tool/approval_needed` shape used on the streaming socket.
pub(crate) fn step_notification(method: &str, params: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// Cap tool output to `STEP_MAX_BYTES` on a char boundary.
/// Returns `(capped_output, was_truncated, full_byte_len)`.
pub(crate) fn cap_step_output(output: &str) -> (String, bool, usize) {
    let full_len = output.len();
    if full_len <= STEP_MAX_BYTES {
        return (output.to_string(), false, full_len);
    }
    let mut cut = STEP_MAX_BYTES;
    while !output.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut s = output[..cut].to_string();
    s.push_str("\n[truncated]");
    (s, true, full_len)
}

pub(super) fn echo_response(input: &Message) -> Message {
    let text = input
        .parts
        .iter()
        .find_map(|p| match p {
            MessagePart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    Message {
        role: "agent".into(),
        parts: vec![MessagePart::Text {
            text: format!("echo: {text}"),
        }],
    }
}

/// Build a plain agent reply carrying `text` verbatim (no model call).
pub(super) fn text_response(text: &str) -> Message {
    Message {
        role: "agent".into(),
        parts: vec![MessagePart::Text { text: text.into() }],
    }
}

/// Separator between two model calls that stream into one on-screen bubble.
/// It is the one `run_agentic_loop` joins the reply's segments with, so what
/// the user watched stream and the reply that replaces it are the same text.
pub(super) const SEGMENT_SEP: &str = "\n\n";

/// A sink that forwards to `out`, opening the first visible (non-thinking,
/// non-empty) delta with [`SEGMENT_SEP`]. Lazily, not up front: a call that
/// streams no text must not leave a dangling separator in the bubble.
///
/// The returned handle finishes once the sender (and every clone the client
/// made) is dropped and everything has been forwarded; await it before
/// writing to `out` directly.
pub(super) fn separated(
    out: tokio::sync::mpsc::Sender<crate::llm::StreamDelta>,
) -> (
    tokio::sync::mpsc::Sender<crate::llm::StreamDelta>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::llm::StreamDelta>(out.max_capacity());
    let fwd = tokio::spawn(async move {
        let mut opened = false;
        while let Some(mut d) = rx.recv().await {
            if !opened && !d.thinking && !d.text.is_empty() {
                opened = true;
                d.text.insert_str(0, SEGMENT_SEP);
            }
            if out.send(d).await.is_err() {
                break;
            }
        }
    });
    (tx, fwd)
}
