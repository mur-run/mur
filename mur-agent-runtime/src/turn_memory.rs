//! What one turn did, as the NEXT turn remembers it.
//!
//! The settlement ledger (`turn_ledger`) tells the user what happened; this
//! module tells the model. The two need different facts: memory needs an
//! `excerpt` for a result the model will otherwise re-run the command to
//! learn, an `attachments` count so "no image was attached" is on the record,
//! and — the load-bearing line — `narrative_only: true` on a turn that ran
//! nothing, so a report produced from zero tool calls stops looking, in
//! memory, exactly like one produced from twelve. Spec:
//! `docs/superpowers/specs/2026-09-19-turn-ledger-memory-design.md`.
//!
//! Re-exported from `turn_ledger`; a separate file only because that module
//! is already past the 800-line rule.

use crate::turn_ledger::{Outcome, RUNAWAY_BACKSTOP, TurnLedger, after_cd_prefix, truncate};
use serde::{Deserialize, Serialize};

/// Tool rows kept per remembered turn; the rest is a count. A turn that made
/// more calls than this is remembered as "25 rows and N more", not as a wall.
pub const MEMORY_ROWS: usize = 25;
/// Opening tag prefix of the rendered memory; `render_memory` completes it
/// with the turn number and `source="runtime"`. Adapters wrap, never format.
pub const MEMORY_OPEN: &str = "<turn_ledger turn=\"";
pub const MEMORY_CLOSE: &str = "</turn_ledger>";

/// One structured line from a successful `bash` result, for memory. Three
/// rows, on purpose: each is a place the model later needs a fact it would
/// otherwise have to re-run the command for. Anything not matched yields
/// `None` — an absent excerpt is honest, a first-line-of-output excerpt is
/// usually a progress bar.
pub fn excerpt_for(command: &str, content: &str) -> Option<String> {
    let cmd = after_cd_prefix(command);
    let picked = if cmd.starts_with("cargo test") || cmd.starts_with("cargo nextest") {
        content
            .lines()
            .map(str::trim)
            .rfind(|l| l.starts_with("test result:"))
            .map(str::to_string)
    } else if cmd.starts_with("gh pr view") || cmd.starts_with("gh pr list") {
        gh_pr_fields(content)
    } else if cmd.starts_with("git status") {
        content
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_string)
    } else {
        None
    };
    picked
        .filter(|s| !s.is_empty())
        .map(|s| truncate(&s, RUNAWAY_BACKSTOP))
}

/// `state` / `mergeable` out of `gh pr view|list` output, JSON or table form.
fn gh_pr_fields(content: &str) -> Option<String> {
    const KEYS: [&str; 2] = ["state", "mergeable"];
    let trimmed = content.trim();
    let mut found: Vec<String> = Vec::new();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        let objects: Vec<&serde_json::Value> = match &v {
            serde_json::Value::Array(a) => a.iter().collect(),
            other => vec![other],
        };
        for o in objects {
            for k in KEYS {
                if let Some(s) = o.get(k).and_then(|x| x.as_str()) {
                    found.push(format!("{k}: {s}"));
                }
            }
        }
    } else {
        for line in trimmed.lines() {
            let line = line.trim();
            for k in KEYS {
                if let Some(rest) = line.strip_prefix(k)
                    && let Some(rest) = rest.strip_prefix(':')
                {
                    found.push(format!("{k}: {}", rest.trim()));
                }
            }
        }
    }
    (!found.is_empty()).then(|| found.join(", "))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolMemoryStatus {
    Ok,
    Failed,
    Denied,
    Running,
}

impl ToolMemoryStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Denied => "denied",
            Self::Running => "running",
        }
    }
}

/// One tool call as the next turn will remember it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolMemory {
    pub tool: String,
    /// `describe_target()` — already the intent-bearing argument.
    pub target: String,
    pub status: ToolMemoryStatus,
    /// `failed` / `denied` only: the detail, already capped by `classify`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `ok` only, and only when [`excerpt_for`] matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<String>,
}

/// What one turn did, as remembered by the next turn. A projection of
/// [`TurnLedger`] plus the facts memory needs and the card does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnMemory {
    /// Images the user attached to this turn's input. `0` is the load-bearing
    /// value: "no image was attached" has to be on the record.
    pub attachments: u32,
    /// `tools.is_empty()`. Redundant on purpose — the rendered line
    /// `narrative_only: true` is the counter-example the model reads.
    pub narrative_only: bool,
    pub tools: Vec<ToolMemory>,
    /// Calls beyond [`MEMORY_ROWS`] dropped from `tools`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub more: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl TurnMemory {
    /// The memory of a turn that ran nothing (single-call paths, stub
    /// backends, a reply whose ledger part was missing or unreadable).
    pub fn empty(attachments: u32) -> Self {
        Self {
            attachments,
            narrative_only: true,
            tools: Vec::new(),
            more: 0,
        }
    }

    pub fn project(ledger: &TurnLedger, attachments: u32) -> Self {
        let tools: Vec<ToolMemory> = ledger
            .actions
            .iter()
            .take(MEMORY_ROWS)
            .map(|a| {
                let (status, error) = match &a.outcome {
                    Outcome::Ok => (ToolMemoryStatus::Ok, None),
                    Outcome::Failed(d) => (ToolMemoryStatus::Failed, Some(d.clone())),
                    Outcome::Denied(d) => (ToolMemoryStatus::Denied, Some(d.clone())),
                    Outcome::Running(_) => (ToolMemoryStatus::Running, None),
                };
                ToolMemory {
                    tool: a.tool.clone(),
                    target: a.target.clone(),
                    status,
                    error,
                    excerpt: if status == ToolMemoryStatus::Ok {
                        a.excerpt.clone()
                    } else {
                        None
                    },
                }
            })
            .collect();
        let more = ledger.actions.len().saturating_sub(MEMORY_ROWS) as u32;
        Self {
            attachments,
            narrative_only: tools.is_empty(),
            tools,
            more,
        }
    }
}

/// Render a memory as the YAML block the model reads. Strings are emitted
/// through `serde_json::to_string`, which is a valid YAML double-quoted
/// scalar — one escaping rule, no hand-rolled quoting.
pub fn render_memory(turn: u32, m: &TurnMemory) -> String {
    fn q(s: &str) -> String {
        serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
    }
    let mut out = format!("{MEMORY_OPEN}{turn}\" source=\"runtime\">\n");
    out.push_str(&format!("attachments: {}\n", m.attachments));
    out.push_str(&format!("narrative_only: {}\n", m.narrative_only));
    if m.tools.is_empty() {
        out.push_str("tools: []\n");
    } else {
        out.push_str("tools:\n");
        for t in &m.tools {
            out.push_str(&format!("  - tool: {}\n", t.tool));
            out.push_str(&format!("    target: {}\n", q(&t.target)));
            out.push_str(&format!("    status: {}\n", t.status.as_str()));
            if let Some(e) = &t.error {
                out.push_str(&format!("    error: {}\n", q(e)));
            }
            if let Some(x) = &t.excerpt {
                out.push_str(&format!("    excerpt: {}\n", q(x)));
            }
        }
    }
    if m.more > 0 {
        out.push_str(&format!("more: {}\n", m.more));
    }
    out.push_str(MEMORY_CLOSE);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::turn_ledger::Action;

    fn act(tool: &str, target: &str, outcome: Outcome) -> Action {
        Action {
            tool: tool.into(),
            target: target.into(),
            outcome,
            excerpt: None,
        }
    }

    #[test]
    fn excerpt_takes_the_cargo_test_result_line() {
        let out = "running 3 tests\ntest a ... ok\ntest result: ok. 3 passed; 0 failed\n";
        assert_eq!(
            excerpt_for("cargo test -p x", out).as_deref(),
            Some("test result: ok. 3 passed; 0 failed")
        );
        assert_eq!(
            excerpt_for("cd /repo && cargo nextest run", out).as_deref(),
            Some("test result: ok. 3 passed; 0 failed")
        );
        // No result line (a build error before the tests ran): nothing.
        assert_eq!(
            excerpt_for("cargo test", "error[E0425]: cannot find value"),
            None
        );
    }

    #[test]
    fn excerpt_takes_gh_pr_state_from_json_and_table_forms() {
        let json = r#"{"state":"MERGED","mergeable":"UNKNOWN"}"#;
        assert_eq!(
            excerpt_for("gh pr view 1402 --json state,mergeable", json).as_deref(),
            Some("state: MERGED, mergeable: UNKNOWN")
        );
        let table = "title:\tfix it\nstate:\tOPEN\nmergeable:\tMERGEABLE\n";
        assert_eq!(
            excerpt_for("gh pr view 1403", table).as_deref(),
            Some("state: OPEN, mergeable: MERGEABLE")
        );
        let list = r#"[{"number":1,"state":"OPEN"},{"number":2,"state":"MERGED"}]"#;
        assert_eq!(
            excerpt_for("gh pr list --json number,state", list).as_deref(),
            Some("state: OPEN, state: MERGED")
        );
        assert_eq!(excerpt_for("gh pr view 1", "no such pull request"), None);
    }

    #[test]
    fn excerpt_takes_the_first_line_of_git_status_and_nothing_else() {
        assert_eq!(
            excerpt_for("git status", "\nOn branch main\nnothing to commit\n").as_deref(),
            Some("On branch main")
        );
        // Not in the table: absent beats noise.
        assert_eq!(excerpt_for("git log --oneline", "abc123 x"), None);
        assert_eq!(excerpt_for("ls -la", "total 0"), None);
        assert_eq!(
            excerpt_for("grep -rn \"cargo test\" src/", "src/a.rs:1: cargo test"),
            None
        );
    }

    #[test]
    fn excerpt_is_capped_by_the_runaway_backstop() {
        let long = format!("test result: ok. {}", "x".repeat(1000));
        let e = excerpt_for("cargo test", &long).unwrap();
        assert!(e.chars().count() <= RUNAWAY_BACKSTOP, "{}", e.len());
        assert!(e.ends_with('…'));
    }

    #[test]
    fn memory_projection_marks_an_empty_turn_narrative_only() {
        let l = TurnLedger::default();
        let m = TurnMemory::project(&l, 0);
        assert!(m.narrative_only);
        assert!(m.tools.is_empty());
        assert_eq!(m.more, 0);
        assert_eq!(m, TurnMemory::empty(0));
        let mut l = TurnLedger::default();
        l.record(act("read_file", "a.txt", Outcome::Ok));
        assert!(!TurnMemory::project(&l, 1).narrative_only);
    }

    #[test]
    fn memory_projection_carries_errors_and_excerpts_by_status() {
        let mut l = TurnLedger::default();
        l.record(act(
            "read_file",
            "/x/info.txt",
            Outcome::Failed("EDEADLK".into()),
        ));
        l.record(act(
            "bash",
            "cargo x",
            Outcome::Denied("not in allowlist".into()),
        ));
        l.record(act("bash", "sleep 900", Outcome::Running("j-1".into())));
        l.record(Action {
            tool: "bash".into(),
            target: "cargo test".into(),
            outcome: Outcome::Ok,
            excerpt: Some("test result: ok. 1 passed".into()),
        });
        let m = TurnMemory::project(&l, 2);
        assert_eq!(m.attachments, 2);
        assert_eq!(m.tools[0].status, ToolMemoryStatus::Failed);
        assert_eq!(m.tools[0].error.as_deref(), Some("EDEADLK"));
        assert_eq!(m.tools[1].status, ToolMemoryStatus::Denied);
        assert_eq!(m.tools[1].error.as_deref(), Some("not in allowlist"));
        assert_eq!(m.tools[2].status, ToolMemoryStatus::Running);
        assert_eq!(m.tools[2].error, None);
        assert_eq!(m.tools[3].status, ToolMemoryStatus::Ok);
        assert_eq!(
            m.tools[3].excerpt.as_deref(),
            Some("test result: ok. 1 passed")
        );
        assert_eq!(m.tools[3].error, None);
    }

    #[test]
    fn memory_projection_caps_rows_and_counts_the_rest() {
        let mut l = TurnLedger::default();
        for i in 0..(MEMORY_ROWS + 7) {
            l.record(act("bash", &format!("echo {i}"), Outcome::Ok));
        }
        let m = TurnMemory::project(&l, 0);
        assert_eq!(m.tools.len(), MEMORY_ROWS);
        assert_eq!(m.more, 7);
        assert_eq!(
            m.tools[0].target, "echo 0",
            "kept the first rows, not the last"
        );
    }

    #[test]
    fn render_memory_prints_the_header_and_zero_attachments() {
        let s = render_memory(42, &TurnMemory::empty(0));
        assert!(
            s.starts_with("<turn_ledger turn=\"42\" source=\"runtime\">\n"),
            "{s}"
        );
        assert!(s.ends_with(&format!("\n{MEMORY_CLOSE}")), "{s}");
        assert!(s.contains("\nattachments: 0\n"), "{s}");
        assert!(s.contains("\nnarrative_only: true\n"), "{s}");
        assert!(s.contains("\ntools: []\n"), "{s}");
        assert!(!s.contains("more:"), "{s}");
    }

    #[test]
    fn render_memory_lists_rows_with_quoted_strings() {
        let mut l = TurnLedger::default();
        l.record(act(
            "read_file",
            "/x/info.txt",
            Outcome::Failed("deadlock \"avoided\"".into()),
        ));
        l.record(Action {
            tool: "bash".into(),
            target: "gh pr view 1402".into(),
            outcome: Outcome::Ok,
            excerpt: Some("state: MERGED".into()),
        });
        let s = render_memory(7, &TurnMemory::project(&l, 1));
        // `concat!`, not `\`-continued lines: a continuation strips the
        // leading spaces the YAML indentation depends on.
        let expected = concat!(
            "<turn_ledger turn=\"7\" source=\"runtime\">\n",
            "attachments: 1\n",
            "narrative_only: false\n",
            "tools:\n",
            "  - tool: read_file\n",
            "    target: \"/x/info.txt\"\n",
            "    status: failed\n",
            "    error: \"deadlock \\\"avoided\\\"\"\n",
            "  - tool: bash\n",
            "    target: \"gh pr view 1402\"\n",
            "    status: ok\n",
            "    excerpt: \"state: MERGED\"\n",
            "</turn_ledger>",
        );
        assert_eq!(s, expected);
    }

    #[test]
    fn render_memory_names_the_overflow() {
        let mut l = TurnLedger::default();
        for i in 0..(MEMORY_ROWS + 2) {
            l.record(act("bash", &format!("echo {i}"), Outcome::Ok));
        }
        let s = render_memory(1, &TurnMemory::project(&l, 0));
        assert!(s.contains("\nmore: 2\n"), "{s}");
    }
}
