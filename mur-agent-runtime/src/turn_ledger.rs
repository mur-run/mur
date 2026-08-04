//! Per-turn settlement ledger: what the turn actually did, assembled from
//! facts the loop already sees.
//!
//! The point is the boundary between *changed* and *verified*. An agent that
//! reports "done" for code it never compiled teaches the user to distrust every
//! later report, and it is the single most common way an agent turn misleads —
//! not by lying, but by collapsing "I edited nine files" into "it works".
//!
//! So the split is derived, not narrated. A tool that MUTATES state (write,
//! edit) lands in `changed`; a tool that EXECUTES something and succeeded lands
//! in `verified`, because a passing command is the only thing on hand that
//! constitutes evidence; anything that failed or was refused lands in
//! `blocked`. No judgement, nothing for a model to round in its own favour.
//!
//! The runtime renders this. The model writes the prose around it — and the
//! prose can be wrong while the table stays honest, which is exactly the
//! property worth having.

use serde::{Deserialize, Serialize};

/// Tools that change state on disk. Everything else is treated as read-only or
/// executing; `bash` is deliberately NOT here — a shell command's outcome is
/// evidence, whereas an edit is only an intention until something runs.
const MUTATING_TOOLS: &[&str] = &["write_file", "edit_file"];

/// How one tool call ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum Outcome {
    Ok,
    /// The tool ran and reported an error.
    Failed(String),
    /// The kernel sandbox refused it. Distinguished from `Failed` because the
    /// remedy is different — a denial is routed or granted, not retried.
    Denied(String),
}

/// One tool call, reduced to what a reader needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    pub tool: String,
    /// The path, command, or fleet this call was about — enough to recognise
    /// it without reprinting the transcript.
    pub target: String,
    pub outcome: Outcome,
}

impl Action {
    fn mutating(&self) -> bool {
        MUTATING_TOOLS.contains(&self.tool.as_str())
    }
}

/// Why the turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopKind {
    /// The model finished on its own.
    EndTurn,
    MaxIterations,
    TokenBudget,
    LoopDetected,
    /// Output hit `max_tokens` mid-thought.
    MaxTokens,
}

impl StopKind {
    /// Did the turn end on its own terms? Anything else means the output may
    /// be incomplete, which a settlement must say out loud — today the runtime
    /// appends that notice as a trailing string, disconnected from whatever
    /// the model claimed a line earlier.
    pub fn is_clean(self) -> bool {
        self == StopKind::EndTurn
    }

    pub fn as_str(self) -> &'static str {
        match self {
            StopKind::EndTurn => "end_turn",
            StopKind::MaxIterations => "iteration cap",
            StopKind::TokenBudget => "token budget",
            StopKind::LoopDetected => "loop detected",
            StopKind::MaxTokens => "max_tokens",
        }
    }
}

/// The turn's accounting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnLedger {
    pub actions: Vec<Action>,
    pub stop: StopKind,
    pub iterations: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Default for TurnLedger {
    fn default() -> Self {
        Self {
            actions: vec![],
            stop: StopKind::EndTurn,
            iterations: 0,
            input_tokens: 0,
            output_tokens: 0,
        }
    }
}

impl TurnLedger {
    pub fn record(&mut self, action: Action) {
        self.actions.push(action);
    }

    /// Commands that ran and succeeded — the only thing here that constitutes
    /// evidence.
    pub fn verified(&self) -> Vec<&Action> {
        self.actions
            .iter()
            .filter(|a| !a.mutating() && a.outcome == Outcome::Ok)
            .collect()
    }

    /// State changed on disk, with nothing yet run against it.
    pub fn changed(&self) -> Vec<&Action> {
        self.actions
            .iter()
            .filter(|a| a.mutating() && a.outcome == Outcome::Ok)
            .collect()
    }

    /// Failed or refused — what did not happen, and why.
    pub fn blocked(&self) -> Vec<&Action> {
        self.actions
            .iter()
            .filter(|a| a.outcome != Outcome::Ok)
            .collect()
    }

    /// Does this turn warrant a settlement?
    ///
    /// A pure question, or a turn that only read files, does not: a three-row
    /// table under a one-line answer is worse than no table. It earns one when
    /// state changed, when something failed, or when the turn did not end on
    /// its own terms — the cases where the user cannot tell from the reply
    /// alone what actually happened.
    pub fn warrants_settlement(&self) -> bool {
        !self.changed().is_empty() || !self.blocked().is_empty() || !self.stop.is_clean()
    }
}

/// Reduce a tool call's input to the one thing worth showing.
///
/// Each tool's own most-identifying argument, falling back to a short rendering
/// of the whole input for tools that aren't special-cased — a settlement is
/// unreadable if half its rows say `{"cwd":null,"timeout_secs":null,…}`.
pub fn describe_target(tool: &str, input: &serde_json::Value) -> String {
    let field = match tool {
        "bash" => "command",
        "write_file" | "edit_file" | "read_file" => "path",
        "fleet_run" => "fleet",
        _ => "",
    };
    let raw = if field.is_empty() {
        input.to_string()
    } else {
        input
            .get(field)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| input.to_string())
    };
    truncate(raw.trim(), 72)
}

fn truncate(s: &str, max: usize) -> String {
    let cleaned: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if cleaned.chars().count() <= max {
        return cleaned;
    }
    let head: String = cleaned.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// Classify a tool result. `is_error` is structural, so failure never has to be
/// guessed — but a sandbox denial is worth separating out, and the only signal
/// for it is the hint the bash tool writes on a kernel EPERM.
pub fn classify(content: &str, is_error: bool) -> Outcome {
    if let Some(i) = content.find("[sandbox]") {
        let rest = &content[i..];
        let line = rest.lines().next().unwrap_or(rest);
        return Outcome::Denied(truncate(line.trim_start_matches("[sandbox] "), 120));
    }
    if is_error {
        return Outcome::Failed(truncate(content.trim(), 120));
    }
    Outcome::Ok
}

/// How many changed files the card names before collapsing to `+N more`.
const CHANGED_SHOWN: usize = 6;

/// Render the settlement card.
///
/// The runtime draws this, not the model. Two reasons: the model would spend
/// tokens on box-drawing and get the alignment wrong, and — the one that
/// matters — a table assembled from the loop's own records cannot be talked
/// around. The prose above it may still overclaim; this will not agree with it.
///
/// Fenced as a code block because every consumer renders the reply as
/// Markdown, where a single newline is a *soft* break: the whole table was
/// being reflowed into one paragraph, which is exactly the "alignment the
/// model would get wrong" that drawing it here was meant to prevent.
pub fn render(ledger: &TurnLedger) -> String {
    let mut out = String::from("\n\n```\n─ settlement ─────────────────────────────\n");

    let verified = ledger.verified();
    if verified.is_empty() {
        // Stated rather than omitted. An empty verified column is the single
        // most useful line here: it is the difference between "changed nine
        // files" and "it works", and leaving the row out lets the reader
        // assume the latter.
        out.push_str("  ✔ verified   (nothing ran — no evidence this works)\n");
    } else {
        out.push_str("  ✔ verified\n");
        for a in &verified {
            out.push_str(&format!("      {} {}\n", a.tool, a.target));
        }
    }

    let changed = ledger.changed();
    if !changed.is_empty() {
        // Deduped by target: the ledger holds one action per edit, so an agent
        // that touched one file four times used to be reported as four changed
        // files — over a list that visibly repeated the same path. An inflated
        // count is the one thing this card cannot afford.
        // ponytail: linear scan, a turn's worth of actions is tiny.
        let mut files: Vec<&str> = Vec::new();
        for a in &changed {
            if !files.contains(&a.target.as_str()) {
                files.push(&a.target);
            }
        }
        out.push_str(&format!("  ~ changed    {} file(s)\n", files.len()));
        for t in files.iter().take(CHANGED_SHOWN) {
            out.push_str(&format!("      {t}\n"));
        }
        if files.len() > CHANGED_SHOWN {
            out.push_str(&format!("      +{} more\n", files.len() - CHANGED_SHOWN));
        }
    }

    let blocked = ledger.blocked();
    if !blocked.is_empty() {
        out.push_str("  ✘ blocked\n");
        for a in &blocked {
            let why = match &a.outcome {
                Outcome::Denied(d) => format!("sandbox: {d}"),
                Outcome::Failed(f) => f.clone(),
                Outcome::Ok => String::new(),
            };
            out.push_str(&format!("      {} — {}\n", a.target, why));
        }
    }

    if !ledger.stop.is_clean() {
        out.push_str(&format!(
            "  ⚠ stopped at {} ({} iterations) — output may be incomplete\n",
            ledger.stop.as_str(),
            ledger.iterations
        ));
    }
    out.push_str("──────────────────────────────────────────\n```");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(tool: &str, target: &str, outcome: Outcome) -> Action {
        Action {
            tool: tool.into(),
            target: target.into(),
            outcome,
        }
    }

    #[test]
    fn edits_are_changed_and_commands_are_evidence() {
        let mut l = TurnLedger::default();
        l.record(act("edit_file", "src/a.rs", Outcome::Ok));
        l.record(act("write_file", "src/b.rs", Outcome::Ok));
        l.record(act("bash", "cargo test", Outcome::Ok));
        // The whole point: nine edits are not a passing build. Editing lands
        // in `changed`; only something that RAN counts as evidence.
        assert_eq!(l.changed().len(), 2);
        assert_eq!(l.verified().len(), 1);
        assert_eq!(l.verified()[0].target, "cargo test");
    }

    #[test]
    fn a_failed_command_is_never_evidence() {
        let mut l = TurnLedger::default();
        l.record(act(
            "bash",
            "cargo test",
            Outcome::Failed("2 failed".into()),
        ));
        assert!(l.verified().is_empty());
        assert_eq!(l.blocked().len(), 1);
    }

    #[test]
    fn settlement_triggers_on_change_failure_or_dirty_stop() {
        // A read-only turn that ended cleanly: no table.
        let mut quiet = TurnLedger::default();
        quiet.record(act("read_file", "README.md", Outcome::Ok));
        assert!(!quiet.warrants_settlement());

        // One edit is enough.
        let mut edited = TurnLedger::default();
        edited.record(act("edit_file", "a.rs", Outcome::Ok));
        assert!(edited.warrants_settlement());

        // So is one failure, with nothing changed.
        let mut failed = TurnLedger::default();
        failed.record(act("bash", "ls", Outcome::Failed("nope".into())));
        assert!(failed.warrants_settlement());

        // And so is a truncated turn that did nothing at all — the user needs
        // to know the output is partial even when the transcript looks calm.
        let truncated = TurnLedger {
            stop: StopKind::MaxIterations,
            ..Default::default()
        };
        assert!(truncated.warrants_settlement());
    }

    #[test]
    fn classify_separates_denial_from_ordinary_failure() {
        let denied = classify(
            "[exit code: 126]\n\n[sandbox] `cargo` is not in agent 'mur''s spawn allowlist\nmore",
            false,
        );
        match denied {
            Outcome::Denied(d) => assert!(d.contains("cargo"), "{d}"),
            other => panic!("expected Denied, got {other:?}"),
        }
        // A denial is not merely a failure: the remedy is to route or grant,
        // not to retry, so it must not be folded into Failed.
        assert!(matches!(
            classify("error: no such file", true),
            Outcome::Failed(_)
        ));
        assert_eq!(classify("all good", false), Outcome::Ok);
    }

    #[test]
    fn render_states_an_empty_verified_column_instead_of_hiding_it() {
        let mut l = TurnLedger::default();
        l.record(act("edit_file", "src/a.rs", Outcome::Ok));
        let card = render(&l);
        // The whole feature in one assertion: nine edits and no test run must
        // not read as success.
        assert!(card.contains("nothing ran"), "{card}");
        assert!(card.contains("1 file(s)"), "{card}");
    }

    #[test]
    fn render_counts_files_not_edits_and_survives_a_markdown_renderer() {
        let mut l = TurnLedger::default();
        for _ in 0..4 {
            l.record(act("edit_file", "src/a.rs", Outcome::Ok));
        }
        l.record(act("edit_file", "src/b.rs", Outcome::Ok));
        let card = render(&l);
        // Four edits to one file is one changed file. The old count said 5 and
        // then listed src/a.rs four times underneath itself.
        assert!(card.contains("2 file(s)"), "{card}");
        assert_eq!(card.matches("src/a.rs").count(), 1, "{card}");
        // Fenced, or every consumer's Markdown pass reflows the rows into one
        // paragraph (a single newline is a soft break in CommonMark).
        assert!(card.starts_with("\n\n```\n"), "{card}");
        assert!(card.ends_with("```"), "{card}");
    }

    #[test]
    fn render_names_the_denial_and_the_truncation() {
        let mut l = TurnLedger {
            stop: StopKind::MaxIterations,
            iterations: 25,
            ..Default::default()
        };
        l.record(act(
            "bash",
            "cargo test",
            Outcome::Denied("`cargo` is not in the spawn allowlist".into()),
        ));
        let card = render(&l);
        assert!(card.contains("sandbox:"), "{card}");
        // The truncation notice belongs IN the settlement, not appended after
        // the model's own claim where the two can contradict each other.
        assert!(card.contains("iteration cap"), "{card}");
        assert!(card.contains("output may be incomplete"), "{card}");
    }

    #[test]
    fn describe_target_picks_the_identifying_argument() {
        let bash = serde_json::json!({"command": "cargo test", "timeout_secs": 600});
        assert_eq!(describe_target("bash", &bash), "cargo test");
        let edit = serde_json::json!({"path": "src/lib.rs", "old_string": "x"});
        assert_eq!(describe_target("edit_file", &edit), "src/lib.rs");
        // Unknown tool: fall back to the whole input rather than an empty cell.
        assert!(!describe_target("mcp__media__x", &serde_json::json!({"q": 1})).is_empty());
        // Long commands are cut so the table stays a table.
        let long = serde_json::json!({"command": "x".repeat(200)});
        assert!(describe_target("bash", &long).chars().count() <= 72);
        // Newlines would break the row.
        let multi = serde_json::json!({"command": "a\nb"});
        assert_eq!(describe_target("bash", &multi), "a b");
    }
}
