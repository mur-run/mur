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

// Memory-side projection of this ledger lives in its own file (800-line
// rule); re-exported here so callers have one path.
pub use crate::turn_memory::{
    MEMORY_CLOSE, MEMORY_OPEN, MEMORY_ROWS, ToolMemory, ToolMemoryStatus, TurnMemory, excerpt_for,
    render_memory,
};

// The gate for the unverified-claim row (spec 2026-09-19-unverified-claim-card).
// Its own file because this one is already past the 800-line rule.
pub use crate::external_state::claims_external_state;

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
    /// The call yielded and the command is still running (the detail is the
    /// job id). Neither evidence nor a failure: the outcome does not exist yet.
    Running(String),
}

/// One tool call, reduced to what a reader needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Action {
    pub tool: String,
    /// The path, command, or fleet this call was about — enough to recognise
    /// it without reprinting the transcript.
    pub target: String,
    pub outcome: Outcome,
    /// One structured line pulled from a successful `bash` result by
    /// [`excerpt_for`] — `test result: ok. 12 passed` — or nothing. Memory
    /// only; the settlement card never prints it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
}

/// Strip one leading `cd <path> && ` — the one composition agents routinely
/// prepend. Nothing else: see `is_evidence` for why deeper shell parsing is
/// deliberately not attempted.
pub(crate) fn after_cd_prefix(command: &str) -> &str {
    let command = command.trim();
    command
        .strip_prefix("cd ")
        .and_then(|rest| rest.split_once("&&"))
        .map(|(_, after)| after.trim())
        .unwrap_or(command)
}

impl Action {
    fn mutating(&self) -> bool {
        MUTATING_TOOLS.contains(&self.tool.as_str())
    }

    /// Did this call actually exercise the change — a build, test, or lint
    /// run — rather than merely succeed at something read-only?
    ///
    /// Ponytail: this only ever looks at `bash` targets, and only credits a
    /// literal `cargo test|build|check|clippy|nextest` (or `npm test`/`pytest`
    /// as a nod to non-Rust repos) when it is what the command *starts with*,
    /// after stripping one optional leading `cd <path> && ` prefix (agents
    /// routinely prepend that). It does not understand shell composition
    /// beyond that one prefix (`&&` chains, `;`, aliases, Makefile targets,
    /// `just`, CI wrapper scripts, or a test binary invoked directly), and a
    /// runner name appearing anywhere but leading position — e.g. inside a
    /// `grep`/`sed` pattern — is deliberately not credited. A command this
    /// helper fails to recognise must fall back to not-evidence, because a
    /// false `verified` is exactly the failure this card exists to prevent —
    /// a missed real gate run only costs a slightly less generous report,
    /// not a false claim of proof.
    fn is_evidence(&self) -> bool {
        if self.tool != "bash" {
            return false;
        }
        const RUNNERS: &[&str] = &[
            "cargo test",
            "cargo build",
            "cargo check",
            "cargo clippy",
            "cargo nextest",
            "npm test",
            "pytest",
        ];
        let command = after_cd_prefix(&self.target);
        RUNNERS.iter().any(|r| command.starts_with(r))
    }
}

/// Why the turn ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopKind {
    /// The model finished on its own.
    EndTurn,
    /// The 10 000-iteration diagnostic ceiling (spec §6) — a runaway.
    MaxIterations,
    /// Retained so ledgers written before 2.79 still deserialise. Never
    /// produced: the token budget is gone.
    TokenBudget,
    LoopDetected,
    /// Output hit `max_tokens` mid-thought.
    MaxTokens,
    /// The model's stream stopped sending and the partial reply was kept
    /// (`StopReason::Interrupted`). Distinct from `MaxTokens`: nothing decided
    /// to stop, the connection went quiet, so token usage for the turn is
    /// unknown. Recorded separately because labelling it `end_turn` would put
    /// a falsehood in a durable audit record (#1287).
    StreamInterrupted,
    /// A model call failed after the user had already been shown text this
    /// turn. The shown text was kept as the reply rather than failing the
    /// turn — failing it discarded something the user had read, so the next
    /// turn could not recall it. `error` is the call's error, kept because
    /// the ledger is an audit record and "end_turn" would be a falsehood.
    LlmFailedAfterOutput {
        error: String,
    },
    /// The unattended deadline passed (spec §3.2).
    Deadline,
    /// No progress for the stuck window; the last three tool calls (§3.5).
    Stuck {
        last_calls: String,
    },
    /// The model kept calling a tool withdrawn earlier in the turn, so no
    /// further iteration could run anything.
    ToolWithdrawn,
}

/// Repeated from `task_runner::ITERATION_CEILING` in prose; a test pins the
/// two equal so the card never names a number the loop does not use.
const ITERATION_CEILING_NOTE: &str = "the 10000-iteration safety ceiling was hit";

impl StopKind {
    /// Did the turn end on its own terms? Anything else means the output may
    /// be incomplete, which a settlement must say out loud — today the runtime
    /// appends that notice as a trailing string, disconnected from whatever
    /// the model claimed a line earlier.
    pub fn is_clean(&self) -> bool {
        *self == StopKind::EndTurn
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            StopKind::EndTurn => "end_turn",
            StopKind::MaxIterations => "iteration ceiling",
            StopKind::TokenBudget => "token budget",
            StopKind::LoopDetected => "loop detected",
            StopKind::MaxTokens => "max_tokens",
            StopKind::StreamInterrupted => "stream interrupted",
            StopKind::LlmFailedAfterOutput { .. } => "model call failed",
            StopKind::Deadline => "deadline",
            StopKind::Stuck { .. } => "stuck",
            StopKind::ToolWithdrawn => "tool withdrawn",
        }
    }

    /// What to do about it. The remedy names the command that exists NOW —
    /// `mur limits <agent>` — because the settlement card is where the user
    /// learns which bound bit.
    pub fn remedy(&self, agent: &str) -> Option<String> {
        Some(match self {
            StopKind::EndTurn => return None,
            StopKind::MaxIterations => format!(
                "{ITERATION_CEILING_NOTE} — this is a runaway, not a setting; report it with the transcript"
            ),
            StopKind::TokenBudget => {
                // Never produced since 2.79 — reaching here means a CURRENT
                // runtime is reading an OLD ledger, so the reader has nothing
                // to upgrade. Say the bound is retired, and name the ones that
                // replaced it, or this line describes a setting that no longer
                // exists and offers no next step.
                format!(
                    "a token budget stopped this turn — that bound was retired in 2.79 and is \
                     kept only to read old ledgers; the live bounds are: mur limits {agent} \
                     (deadline / stuck / cost_usd)"
                )
            }
            StopKind::LoopDetected => {
                "the last tool call repeated with identical arguments — change the ask, or the tool's input"
                    .to_string()
            }
            StopKind::MaxTokens => {
                "the model's output limit — ask it to continue from where it stopped".to_string()
            }
            StopKind::StreamInterrupted => {
                "the model stopped sending mid-reply — ask it again; if this repeats on a slow \
                 local model, raise MUR_LLM_IDLE_TIMEOUT_SECS for that agent"
                    .to_string()
            }
            StopKind::LlmFailedAfterOutput { error } => format!(
                "the model call failed after the text above was shown ({error}) — ask it to continue"
            ),
            StopKind::Deadline => format!(
                "raise it: mur limits {agent} --deadline <1h>  (or --deadline on the fleet that launched it)"
            ),
            StopKind::Stuck { last_calls } => format!(
                "no progress; last calls: {last_calls} — change the ask, or widen it: mur limits {agent} --stuck <20m|off>"
            ),
            StopKind::ToolWithdrawn => format!(
                "a tool was refused and withdrawn for the turn, and the model kept calling it — \
                 allow it and retry: mur agent perm tool-allow {agent} <tool>"
            ),
        })
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
    /// Whose turn this was — the remedy names `mur limits <agent>`. Empty on
    /// ledgers written before 2.79 and on stub runners.
    #[serde(default)]
    pub agent: String,
    /// The reply text carried external-state evidence — a SHA, a PR number,
    /// a test tally, a diff ([`claims_external_state`]). Set by `settle`;
    /// `unverified_claim()` is this AND no actions. Additive: absent on
    /// ledgers written before it existed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub claims_external_state: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Default for TurnLedger {
    fn default() -> Self {
        Self {
            actions: vec![],
            stop: StopKind::EndTurn,
            iterations: 0,
            input_tokens: 0,
            output_tokens: 0,
            agent: String::new(),
            claims_external_state: false,
        }
    }
}

impl TurnLedger {
    pub fn record(&mut self, action: Action) {
        self.actions.push(action);
    }

    /// Commands that ran, succeeded, and are recognisable gate runs (test,
    /// build, check, or lint) — the only thing here that constitutes
    /// evidence. A successful `cat` or `grep` is not evidence and must not
    /// count.
    pub fn verified(&self) -> Vec<&Action> {
        self.actions
            .iter()
            .filter(|a| !a.mutating() && a.outcome == Outcome::Ok && a.is_evidence())
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
            .filter(|a| !matches!(a.outcome, Outcome::Ok | Outcome::Running(_)))
            .collect()
    }

    /// Yielded and still running — work whose outcome the turn does not know.
    pub fn running(&self) -> Vec<&Action> {
        self.actions
            .iter()
            .filter(|a| matches!(a.outcome, Outcome::Running(_)))
            .collect()
    }

    /// What a resumer needs, derived from the ledger rather than narrated.
    ///
    /// `graceful_exit` already asks the model to "summarize what you
    /// completed … and the remaining steps", but that is prose: it is written
    /// by the same model whose turn just hit a wall, it can round in its own
    /// favour, and nothing checks it. The facts that actually decide whether
    /// work can continue are all here already — which files are dirty, whether
    /// anything was run against them, and what the last failure said. So they
    /// are assembled, not requested.
    ///
    /// `None` for a clean stop: there is nothing to resume, and a handoff
    /// printed under every turn is a handoff nobody reads.
    ///
    /// Ponytail: this deliberately does NOT try to say what to do next. It
    /// reports state, because state is what the ledger knows; the next step is
    /// a judgement, and a judgement asserted by the runtime would be the same
    /// unchecked claim in a different voice.
    pub fn handoff(&self) -> Option<String> {
        if self.stop.is_clean() {
            return None;
        }
        let mut out = String::from("resume from here:\n");

        let changed = self.changed();
        if changed.is_empty() {
            out.push_str("  - nothing on disk changed this turn\n");
        } else {
            let mut files: Vec<&str> = Vec::new();
            for a in &changed {
                if !files.contains(&a.target.as_str()) {
                    files.push(&a.target);
                }
            }
            out.push_str("  - edited, not yet proven:\n");
            for f in &files {
                out.push_str(&format!("      {f}\n"));
            }
        }

        let verified = self.verified();
        if verified.is_empty() {
            out.push_str("  - no gate run succeeded, so none of the above is verified\n");
        } else {
            out.push_str("  - last passing gate:\n");
            for a in verified.iter().rev().take(1) {
                out.push_str(&format!("      {}\n", a.target));
            }
        }

        // The last failure is the single most load-bearing line for whoever
        // picks this up: it is where the next turn starts.
        if let Some(a) = self.blocked().last() {
            let why = match &a.outcome {
                Outcome::Denied(d) => format!("sandbox: {d}"),
                Outcome::Failed(f) => clean_reason(f),
                Outcome::Ok | Outcome::Running(_) => String::new(),
            };
            out.push_str(&format!("  - last failure: {} · {why}\n", a.target));
        }

        for a in self.running() {
            if let Outcome::Running(job) = &a.outcome {
                out.push_str(&format!(
                    "  - still running, outcome unknown: {} ({job})\n",
                    a.target
                ));
            }
        }

        out.push_str(&format!(
            "  - stopped at {} after {} iterations",
            self.stop.as_str(),
            self.iterations
        ));
        Some(out)
    }

    /// Does this turn warrant a settlement?
    ///
    /// A pure question, or a turn that only read files, does not: a three-row
    /// table under a one-line answer is worse than no table. It earns one when
    /// state changed, when something failed, or when the turn did not end on
    /// its own terms — the cases where the user cannot tell from the reply
    /// alone what actually happened. And (2026-09-19) when nothing ran yet the
    /// reply names external state: a report with no evidence behind it.
    pub fn warrants_settlement(&self) -> bool {
        !self.changed().is_empty()
            || !self.blocked().is_empty()
            || !self.running().is_empty()
            || !self.stop.is_clean()
            || self.unverified_claim()
    }

    /// Ran nothing, yet the reply names external state (a SHA, a PR, a tally).
    pub fn unverified_claim(&self) -> bool {
        self.actions.is_empty() && self.claims_external_state
    }
}

/// Reduce a tool call's input to the one thing worth showing.
///
/// Each tool's own most-identifying argument, falling back to a short rendering
/// of the whole input for tools that aren't special-cased — a settlement is
/// unreadable if half its rows say `{"cwd":null,"timeout_secs":null,…}`.
pub fn describe_target(tool: &str, input: &serde_json::Value) -> String {
    describe_target_with_result(tool, input, "")
}

/// Reduce a tool call to a durable, recoverable identity.
///
/// Most tools are identified entirely by their input. Dispatch tools are the
/// exception: the useful identity is allocated by the executor and only
/// appears in the result. Keep that handle in the ledger so a later turn can
/// poll the job even if the conversational transcript is compacted.
pub fn describe_target_with_result(tool: &str, input: &serde_json::Value, result: &str) -> String {
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
    let raw = raw.trim();
    // No-argument calls (`{}`) carry no identifying target — an empty cell
    // reads better on the card than JSON punctuation.
    if raw == "{}" || raw == "null" {
        return String::new();
    }
    let target = truncate(raw, RUNAWAY_BACKSTOP);
    if tool != "fleet_run" {
        return target;
    }

    let Ok(handle) = serde_json::from_str::<serde_json::Value>(result) else {
        return target;
    };
    if handle.get("status").and_then(|v| v.as_str()) != Some("dispatched") {
        return target;
    }
    let Some(run_id) = handle
        .get("run_id")
        .and_then(|v| v.as_str())
        .filter(|id| !id.trim().is_empty())
    else {
        return target;
    };
    truncate(&format!("{target} → {run_id}"), RUNAWAY_BACKSTOP)
}

/// `mcp__server__tool` → `tool`. The server prefix is routing, not identity —
/// on the card it only pushes the name the user knows off the line.
fn short_tool(tool: &str) -> &str {
    tool.strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
        .map(|(_, t)| t)
        .unwrap_or(tool)
}

/// Strip the transport's wrapping from a failure so the card shows the reason,
/// not the plumbing: "tool error: tool execution failed: X" → "X". The ledger
/// keeps the raw detail; this is display-only.
fn clean_reason(why: &str) -> String {
    let mut s = why.trim();
    loop {
        let t = s
            .trim_start_matches("tool error:")
            .trim_start_matches("tool execution failed:")
            .trim_start();
        if t == s {
            break;
        }
        s = t;
    }
    truncate(s, RUNAWAY_BACKSTOP)
}

/// The only length limit the runtime still applies. Not a display width — it
/// exists so one runaway error dump cannot flood the reply. The renderer owns
/// what fits, because it is the only thing that knows the pane.
pub(crate) const RUNAWAY_BACKSTOP: usize = 400;

pub(crate) fn truncate(s: &str, max: usize) -> String {
    let cleaned: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if cleaned.chars().count() <= max {
        return cleaned;
    }
    let head: String = cleaned.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

/// Classify a tool result. `is_error` is structural, so failure never has to be
/// guessed — and so is a sandbox denial: it comes from the tool's own
/// `ToolStatus`, not from sniffing the output text for a hint.
pub fn classify(content: &str, is_error: bool, status: &crate::tools::ToolStatus) -> Outcome {
    if let crate::tools::ToolStatus::Denied { detail, .. } = status {
        return Outcome::Denied(truncate(detail, RUNAWAY_BACKSTOP));
    }
    // Before the `is_error` check: a yield is never an error, and it must not
    // be mistaken for `Ok` — "the tests are still running" is not "the tests
    // passed" (spec §3.7).
    if let crate::tools::ToolStatus::Running { job_id, .. } = status {
        return Outcome::Running(job_id.clone());
    }
    if is_error {
        return Outcome::Failed(truncate(content.trim(), RUNAWAY_BACKSTOP));
    }
    Outcome::Ok
}

/// How many changed files the card names before collapsing to `+N more`.
const CHANGED_SHOWN: usize = 6;

/// The verified row for a turn that ran nothing yet named external state.
/// Same `⚠` as the "nothing ran" row, so the TUI paints it muted: a warning
/// about missing evidence, not a failure.
pub const UNVERIFIED_ROW: &str =
    "  ⚠ unverified  no tool ran this turn — external state claims above were not checked\n";

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
    let mut out = String::from("\n\n```\n─ settlement ─\n");

    let verified = ledger.verified();
    if verified.is_empty() {
        if ledger.unverified_claim() {
            out.push_str(UNVERIFIED_ROW);
        } else {
            // Stated rather than omitted. An empty verified column is the single
            // most useful line here: it is the difference between "changed nine
            // files" and "it works", and leaving the row out lets the reader
            // assume the latter.
            // `⚠`, not `✔`: the TUI colours settlement rows by their lead glyph
            // (`settlement.rs::row_style`), so a success glyph painted this row
            // GREEN and the parenthetical lost the argument to the colour. Not
            // `✘` either — verification was not attempted and failed, it was
            // never run, which is a warning about the evidence, not a failure.
            out.push_str("  ⚠ verified   nothing ran — no evidence this works\n");
        }
    } else {
        // One line per action: the glyph carries "verified"; a group header
        // would only push the content into a second indent level.
        for a in &verified {
            let tool = short_tool(&a.tool);
            if a.target.is_empty() || a.target == tool {
                out.push_str(&format!("  ✔ {tool}\n"));
            } else {
                out.push_str(&format!("  ✔ {tool} · {}\n", a.target));
            }
        }
    }

    // Yielded calls get their own glyph: not ✔ (nothing is proven yet), not ✘
    // (nothing failed). The remedy is on the line because it is always the same.
    for a in ledger.running() {
        let tool = short_tool(&a.tool);
        let job = match &a.outcome {
            Outcome::Running(j) => j.as_str(),
            _ => "",
        };
        out.push_str(&format!(
            "  ⏳ {tool} · still running ({job}) — bash_wait to continue\n"
        ));
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
        // Reason on the tool line (transport noise stripped), target on its
        // own indented line — keeping them separate makes long details easier
        // for a width-aware renderer to reflow.
        for a in &blocked {
            let why = match &a.outcome {
                Outcome::Denied(d) => format!("sandbox: {d}"),
                Outcome::Failed(f) => clean_reason(f),
                Outcome::Ok | Outcome::Running(_) => String::new(),
            };
            let tool = short_tool(&a.tool);
            out.push_str(&format!("  ✘ {tool} · {why}\n"));
            // Skip the target line when the reason already names it. Tools that
            // fail on a path usually quote that path in their message, so this
            // printed it twice — invisible while `clean_reason` truncated at 80
            // chars (the cap cut the path back out), obvious once the card
            // stopped truncating. Fall back to printing it whenever the reason
            // does not contain it verbatim: repeating the target is a cosmetic
            // wart, losing it is a missing fact.
            if !a.target.is_empty() && a.target != tool && !why.contains(a.target.as_str()) {
                out.push_str(&format!("      {}\n", a.target));
            }
        }
    }

    if !ledger.stop.is_clean() {
        out.push_str(&format!(
            "  ⚠ stopped at {} ({} iterations) — output may be incomplete",
            ledger.stop.as_str(),
            ledger.iterations
        ));
        if let Some(r) = ledger.stop.remedy(&ledger.agent) {
            out.push_str(&format!(" · {r}"));
        }
        out.push('\n');
        // Directly under the "output may be incomplete" line, because that is
        // the line that raises the question this answers: incomplete from
        // where? Derived from the ledger, so the model's prose above cannot
        // quietly disagree with it.
        if let Some(h) = ledger.handoff() {
            out.push('\n');
            for line in h.lines() {
                out.push_str(&format!("  {line}\n"));
            }
        }
    }
    out.push_str("```");
    out
}

#[cfg(test)]
mod tests;
