# Plan: turn ledger in multi-turn memory

**Spec:** `docs/superpowers/specs/2026-09-19-turn-ledger-memory-design.md` (read it first; this plan is its mechanical form).
**Execution skill:** `mur-executing-plans` (single crate, five tasks, sequential — no delegation needed).
**Branch:** `spec/turn-ledger-memory` is the spec; implement on `fix/turn-ledger-memory` branched from `origin/main` in a worktree under `.worktrees/`.

**Goal:** every remembered turn carries a structured record of what it did (tool rows, failures verbatim, `narrative_only` when nothing ran, `attachments` count), stored as its own `RichMessage` variant and rendered to the model as a runtime-attributed user-role message.

**Architecture:** the agentic loop's existing `TurnLedger` is projected into a `TurnMemory` at the end of every turn, travels to `remember_turn` inside the reply's `application/vnd.mur.turn-ledger+json` Data part (now attached unconditionally), and is stored as the third element of each remembered turn `[user, agent, TurnLedger]`. Adapters render the variant; the store trims by whole turns.

**Tech stack:** Rust 2024, `serde`/`serde_json`, `cargo nextest`. Crate: `mur-agent-runtime` only.

## Global Constraints (from the spec — every task includes these)

- Memory never fails a turn: a missing or malformed ledger part yields a synthesized `narrative_only: true` memory, never an error.
- Absent beats noise: an `excerpt` exists only for a `bash` call with `Outcome::Ok` whose command matches one row of the three-row table; otherwise the field is omitted.
- `error` and `excerpt` are each ≤ `RUNAWAY_BACKSTOP` (400 chars, existing constant); at most `MEMORY_ROWS = 25` tool rows per turn, overflow counted in `more`.
- The variant is rendered as ONE user-role text message wrapped in the header constants; adapters wrap, `render_memory` formats. Never folded into assistant text.
- The stored turn is the triple `[user Text|ImageText → downgraded to Text as today, agent Text, TurnLedger]`; trimming removes whole turns and the history always starts on a user-role message.
- `warrants_settlement()` and the rendered settlement card are unchanged. Old conversation files are not migrated.
- `attachments` is counted with the same predicate `user_message` uses (`MessagePart::Data` whose `mime_type` starts with `image/`), through one shared helper.
- Commands: `cargo nextest run -p mur-agent-runtime <filter>` for tests; `cargo clippy -p mur-agent-runtime --all-targets --no-deps -- -D warnings` and `cargo fmt -p mur-agent-runtime` before every commit. Read exit codes, not grep output.
- Commit messages end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.

## File structure

| File | Responsibility in this change |
|---|---|
| `mur-agent-runtime/src/turn_ledger.rs` | `Action.excerpt`; `after_cd_prefix` (shared by `is_evidence` and the excerpt table); `excerpt_for`; `TurnMemory`/`ToolMemory`/`ToolMemoryStatus`; `MEMORY_ROWS`; `TurnMemory::project`; `MEMORY_OPEN`/`MEMORY_CLOSE`; `render_memory`. Tests for all of them. |
| `mur-agent-runtime/src/llm/mod.rs` | `RichMessage::TurnLedger { turn, memory }`. |
| `mur-agent-runtime/src/llm/anthropic.rs` | Render the variant as a user text block (coalesced with the following user message by the existing `push_coalesced`). Test. |
| `mur-agent-runtime/src/llm/openai/mod.rs`, `llm/openai/tests.rs` | Render the variant as `{"role":"user","content":…}`. Test. |
| `mur-agent-runtime/src/llm/ollama.rs` | Render the variant as `{"role":"user","content":…}`. Test. |
| `mur-agent-runtime/src/llm/fallback/mod.rs` | `task_summary` skips the variant; `estimate_input_tokens` counts its rendered length. Test. |
| `mur-agent-runtime/src/task_runner.rs` | Record site fills `excerpt`; `settle` attaches the Data part every turn; `image_count`; `remember_turn` stores the triple (ledger from the Data part, else synthesized); `estimated_tokens` counts the variant; `ConversationStore::remember` trims by whole turn. Tests, including the incident regression lock. |
| `docs/superpowers/plans/2026-09-19-turn-ledger-memory-plan.md` | This file. |

---

## Task 1 — `turn_ledger.rs`: projection type, excerpt table, renderer

**Interfaces — Produces** (later tasks rely on exact names):

```rust
// turn_ledger.rs
pub struct Action { pub tool: String, pub target: String, pub outcome: Outcome,
                    pub excerpt: Option<String> }            // new field, #[serde(default, skip_serializing_if = "Option::is_none")]
pub const MEMORY_ROWS: usize = 25;
pub const MEMORY_OPEN: &str = "<turn_ledger turn=\"";       // render_memory writes `{MEMORY_OPEN}{turn}\" source=\"runtime\">`
pub const MEMORY_CLOSE: &str = "</turn_ledger>";
pub fn excerpt_for(command: &str, content: &str) -> Option<String>;
pub enum ToolMemoryStatus { Ok, Failed, Denied, Running }    // serde rename_all = "lowercase"
pub struct ToolMemory { pub tool: String, pub target: String, pub status: ToolMemoryStatus,
                        pub error: Option<String>, pub excerpt: Option<String> }
pub struct TurnMemory { pub attachments: u32, pub narrative_only: bool,
                        pub tools: Vec<ToolMemory>, pub more: u32 }
impl TurnMemory { pub fn project(ledger: &TurnLedger, attachments: u32) -> Self;
                  pub fn empty(attachments: u32) -> Self; }
pub fn render_memory(turn: u32, m: &TurnMemory) -> String;
```

- [ ] **Step 1.1 — add the field and the shared prefix helper.** In `turn_ledger.rs` replace the `Action` struct and the body of `is_evidence` as follows.

```rust
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
fn after_cd_prefix(command: &str) -> &str {
    let command = command.trim();
    command
        .strip_prefix("cd ")
        .and_then(|rest| rest.split_once("&&"))
        .map(|(_, after)| after.trim())
        .unwrap_or(command)
}
```

In `is_evidence`, replace the four lines from `let command = self.target.trim();` through `.unwrap_or(command);` with:

```rust
        let command = after_cd_prefix(&self.target);
```

Then add `excerpt: None,` to the `act` helper in the test module (`Action { tool, target, outcome, excerpt: None }`).

- [ ] **Step 1.2 — run the existing tests; they must still pass.**

```
cargo nextest run -p mur-agent-runtime turn_ledger
```
Expected: all `turn_ledger::tests::*` pass (the `cd` prefix test in particular).

- [ ] **Step 1.3 — write the excerpt tests (red).** Append inside `mod tests`:

```rust
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
        assert_eq!(excerpt_for("cargo test", "error[E0425]: cannot find value"), None);
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
        assert_eq!(excerpt_for("grep -rn \"cargo test\" src/", "src/a.rs:1: cargo test"), None);
    }

    #[test]
    fn excerpt_is_capped_by_the_runaway_backstop() {
        let long = format!("test result: ok. {}", "x".repeat(1000));
        let e = excerpt_for("cargo test", &long).unwrap();
        assert!(e.chars().count() <= RUNAWAY_BACKSTOP, "{}", e.len());
        assert!(e.ends_with('…'));
    }
```

- [ ] **Step 1.4 — watch them fail.**

```
cargo nextest run -p mur-agent-runtime excerpt_
```
Expected: compile error `cannot find function excerpt_for`.

- [ ] **Step 1.5 — implement `excerpt_for`.** Add above `/// How many changed files the card names` (i.e. after `classify`):

```rust
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
            .filter(|l| l.starts_with("test result:"))
            .last()
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
```

- [ ] **Step 1.6 — watch them pass.**

```
cargo nextest run -p mur-agent-runtime excerpt_
```
Expected: 4 passed.

- [ ] **Step 1.7 — write the projection and renderer tests (red).** Append inside `mod tests`:

```rust
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
        l.record(act("read_file", "/x/info.txt", Outcome::Failed("EDEADLK".into())));
        l.record(act("bash", "cargo x", Outcome::Denied("not in allowlist".into())));
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
        assert_eq!(m.tools[3].excerpt.as_deref(), Some("test result: ok. 1 passed"));
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
        assert_eq!(m.tools[0].target, "echo 0", "kept the first rows, not the last");
    }

    #[test]
    fn render_memory_prints_the_header_and_zero_attachments() {
        let s = render_memory(42, &TurnMemory::empty(0));
        assert!(s.starts_with("<turn_ledger turn=\"42\" source=\"runtime\">\n"), "{s}");
        assert!(s.ends_with(&format!("\n{MEMORY_CLOSE}")), "{s}");
        assert!(s.contains("\nattachments: 0\n"), "{s}");
        assert!(s.contains("\nnarrative_only: true\n"), "{s}");
        assert!(s.contains("\ntools: []\n"), "{s}");
        assert!(!s.contains("more:"), "{s}");
    }

    #[test]
    fn render_memory_lists_rows_with_quoted_strings() {
        let mut l = TurnLedger::default();
        l.record(act("read_file", "/x/info.txt", Outcome::Failed("deadlock \"avoided\"".into())));
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
```

- [ ] **Step 1.8 — watch them fail.**

```
cargo nextest run -p mur-agent-runtime memory_projection render_memory
```
Expected: compile errors for `TurnMemory`, `render_memory`, `MEMORY_ROWS`, `MEMORY_CLOSE`.

- [ ] **Step 1.9 — implement the types and renderer.** Add after `excerpt_for`/`gh_pr_fields` (before `/// How many changed files`):

```rust
/// Tool rows kept per remembered turn; the rest is a count. A turn that made
/// more calls than this is remembered as "25 rows and N more", not as a wall.
pub const MEMORY_ROWS: usize = 25;
/// Opening tag prefix of the rendered memory; `render_memory` completes it
/// with the turn number and `source="runtime"`. Adapters wrap, never format.
pub const MEMORY_OPEN: &str = "<turn_ledger turn=\"";
pub const MEMORY_CLOSE: &str = "</turn_ledger>";

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
```

- [ ] **Step 1.10 — watch them pass, lint, commit.**

```
cargo nextest run -p mur-agent-runtime turn_ledger
cargo clippy -p mur-agent-runtime --all-targets --no-deps -- -D warnings
cargo fmt -p mur-agent-runtime
```
Expected: all `turn_ledger` tests pass (existing + 10 new); clippy exit 0. Note `Action { … }` is also constructed at `task_runner.rs` ≈ line 2375 — clippy will fail there with `missing field excerpt`. Add `excerpt: None,` to that one site for now (Task 3 replaces it). Commit:

```
git add -A && git commit -m "feat(turn-ledger): TurnMemory projection, excerpt table, renderer

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 2 — `RichMessage::TurnLedger` and the adapters

**Interfaces — Consumes:** `turn_ledger::{TurnMemory, render_memory, MEMORY_OPEN, MEMORY_CLOSE}` from Task 1.
**Interfaces — Produces:**

```rust
// llm/mod.rs
RichMessage::TurnLedger { turn: u32, memory: crate::turn_ledger::TurnMemory }
```
Every adapter renders it as a user-role text whose content is exactly `render_memory(turn, memory)`.

- [ ] **Step 2.1 — add the variant.** In `llm/mod.rs`, inside `pub enum RichMessage`, after the `ImageText { … }` arm:

```rust
    /// The runtime's record of what the preceding assistant turn did (spec
    /// 2026-09-19-turn-ledger-memory). Written by `remember_turn`, never by a
    /// provider; rendered as a user-role text block under a fixed header so
    /// the model reads it as testimony about itself, not as its own prose.
    TurnLedger {
        turn: u32,
        memory: crate::turn_ledger::TurnMemory,
    },
```

- [ ] **Step 2.2 — make it compile.** Run `cargo check -p mur-agent-runtime --all-targets`. Fix each non-exhaustive match:

`llm/anthropic.rs` `rich_messages_to_anthropic`, add an arm before the closing of `match m`:
```rust
            RichMessage::TurnLedger { turn, memory } => {
                push_coalesced(
                    &mut convo,
                    "user",
                    json!(crate::turn_ledger::render_memory(*turn, memory)),
                );
            }
```

`llm/openai/mod.rs` `rich_messages_to_openai`, add an arm:
```rust
            RichMessage::TurnLedger { turn, memory } => {
                result.push(json!({
                    "role": "user",
                    "content": crate::turn_ledger::render_memory(*turn, memory),
                }));
            }
```

`llm/ollama.rs` `to_ollama_messages`, replace `_ => None,` with:
```rust
            RichMessage::TurnLedger { turn, memory } => Some(json!({
                "role": "user",
                "content": crate::turn_ledger::render_memory(*turn, memory),
            })),
            RichMessage::ToolUse { .. } | RichMessage::ToolResults { .. } => None,
```

`llm/fallback/mod.rs` `estimate_input_tokens`, add an arm:
```rust
            RichMessage::TurnLedger { turn, memory } => {
                crate::turn_ledger::render_memory(*turn, memory).len()
            }
```
(`task_summary` and `requirements_of` already use `_ =>`; no change — `task_summary` therefore skips the variant by construction. The test in Step 2.3 locks that.)

`task_runner.rs` `estimated_tokens` (≈ line 143), add an arm:
```rust
            M::TurnLedger { turn, memory } => {
                crate::turn_ledger::render_memory(*turn, memory).len()
            }
```

`task_runner.rs` test ≈ line 5972: change `RichMessage::Text { .. } | RichMessage::ImageText { .. } => {}` to `RichMessage::Text { .. } | RichMessage::ImageText { .. } | RichMessage::TurnLedger { .. } => {}`.

Expected after fixes: `cargo check -p mur-agent-runtime --all-targets` exit 0.

- [ ] **Step 2.3 — write the adapter tests (red would be "compile ok, assertion fails" — they are written against the arms above, so write them and run once).**

`llm/anthropic.rs`, inside `mod tests`, after `rich_messages_tool_use_and_results`:
```rust
    /// A ledger is one user text block, and it coalesces with the user
    /// message that follows it — Anthropic 400s on consecutive user turns.
    #[test]
    fn turn_ledger_renders_as_user_text_coalesced_with_the_next_message() {
        let memory = crate::turn_ledger::TurnMemory::empty(0);
        let msgs = vec![
            RichMessage::Text { role: "user".into(), content: "do it".into() },
            RichMessage::Text { role: "agent".into(), content: "done".into() },
            RichMessage::TurnLedger { turn: 3, memory: memory.clone() },
            RichMessage::Text { role: "user".into(), content: "really?".into() },
        ];
        let (_, convo, _) = rich_messages_to_anthropic(&msgs);
        assert_eq!(convo.len(), 3, "{convo:?}");
        assert_eq!(convo[2]["role"], "user");
        let blocks = convo[2]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        let rendered = crate::turn_ledger::render_memory(3, &memory);
        assert_eq!(blocks[0]["text"], rendered);
        assert!(rendered.starts_with(crate::turn_ledger::MEMORY_OPEN));
        assert_eq!(blocks[1]["text"], "really?");
    }
```

`llm/openai/tests.rs`, append:
```rust
#[test]
fn turn_ledger_renders_as_a_user_message() {
    let memory = crate::turn_ledger::TurnMemory::empty(1);
    let msgs = vec![
        RichMessage::Text { role: "agent".into(), content: "done".into() },
        RichMessage::TurnLedger { turn: 9, memory: memory.clone() },
    ];
    let out = rich_messages_to_openai(&msgs);
    assert_eq!(out.len(), 2);
    assert_eq!(out[1]["role"], "user");
    assert_eq!(out[1]["content"], crate::turn_ledger::render_memory(9, &memory));
    assert!(out[1]["content"].as_str().unwrap().ends_with(crate::turn_ledger::MEMORY_CLOSE));
}
```

`llm/ollama.rs`, inside `mod tests`, after `to_ollama_messages_drops_tool_messages`:
```rust
    #[test]
    fn to_ollama_messages_renders_a_turn_ledger_as_user_text() {
        let memory = crate::turn_ledger::TurnMemory::empty(0);
        let msgs = vec![RichMessage::TurnLedger { turn: 2, memory: memory.clone() }];
        let out = to_ollama_messages(&msgs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["content"], crate::turn_ledger::render_memory(2, &memory));
    }
```

`llm/fallback/tests.rs` (the module's tests live there and already `use super::*;`, which brings `task_summary` and `estimate_input_tokens` into scope), append:
```rust
#[test]
fn task_summary_skips_a_turn_ledger_and_tokens_count_it() {
    use crate::llm::{LlmRequest, RichMessage};
    let memory = crate::turn_ledger::TurnMemory::empty(0);
    let req = LlmRequest {
        messages: vec![
            RichMessage::TurnLedger { turn: 1, memory: memory.clone() },
            RichMessage::Text { role: "user".into(), content: "hello there".into() },
        ],
        ..Default::default()
    };
    assert_eq!(task_summary(&req), "hello there");
    let rendered = crate::turn_ledger::render_memory(1, &memory).len();
    assert_eq!(
        estimate_input_tokens(&req) as usize,
        (rendered + "hello there".len()) / 4
    );
}
```

- [ ] **Step 2.4 — run, lint, commit.**

```
cargo nextest run -p mur-agent-runtime turn_ledger_renders to_ollama_messages_renders task_summary_skips
cargo clippy -p mur-agent-runtime --all-targets --no-deps -- -D warnings
cargo fmt -p mur-agent-runtime
```
Expected: 4 passed; clippy exit 0. Commit:

```
git add -A && git commit -m "feat(llm): RichMessage::TurnLedger, rendered as runtime-attributed user text

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 3 — `task_runner.rs`: produce, hand off, remember the triple

**Interfaces — Consumes:** Task 1 types; Task 2 variant.
**Interfaces — Produces:**

```rust
// task_runner.rs (private)
const TURN_LEDGER_MIME: &str = "application/vnd.mur.turn-ledger+json";
fn image_count(input: &Message) -> u32;
fn ledger_of(reply: &Message) -> Option<crate::turn_ledger::TurnLedger>;
// remember_turn now stores [user, agent, TurnLedger{turn, memory}]
```

- [ ] **Step 3.1 — the record site fills `excerpt`.** At ≈ line 2375 replace the `ledger.record(…)` call with:

```rust
                let outcome = crate::turn_ledger::classify(
                    &entry.content,
                    entry.is_error,
                    &entry.status,
                );
                let excerpt = if call.tool_name == "bash"
                    && outcome == crate::turn_ledger::Outcome::Ok
                {
                    crate::turn_ledger::excerpt_for(
                        &crate::turn_ledger::describe_target(&call.tool_name, &call.input),
                        &entry.content,
                    )
                } else {
                    None
                };
                ledger.record(crate::turn_ledger::Action {
                    tool: call.tool_name.clone(),
                    target: crate::turn_ledger::describe_target(&call.tool_name, &call.input),
                    outcome,
                    excerpt,
                });
```
(Remove the `excerpt: None,` placeholder from Task 1 Step 1.10.)

- [ ] **Step 3.2 — `settle` attaches the ledger every turn.** Replace `fn settle` (≈ line 2622) with:

```rust
/// MIME of the per-turn ledger Data part on every reply. No client renders
/// it today (grepped 2026-09-19); `remember_turn` reads it back, and the Hub
/// gets a free per-turn record when it wants one.
const TURN_LEDGER_MIME: &str = "application/vnd.mur.turn-ledger+json";

fn settle(text: String, ledger: &crate::turn_ledger::TurnLedger) -> Message {
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
```

- [ ] **Step 3.3 — write the `remember_turn` tests (red).** In the task_runner `mod tests`, after `threads_multi_turn_chat_memory`:

```rust
    #[tokio::test]
    async fn a_stub_turn_is_remembered_with_a_narrative_only_ledger() {
        use crate::llm::RichMessage;
        let runner = TaskRunner::new_stub_echo();
        let _ = runner.run_sync(user_turn("first", "t1", None)).await;
        let store = runner.conversations.lock().unwrap();
        let h = store.map.get("t1").expect("remembered");
        assert_eq!(h.len(), 3, "user, agent, ledger: {h:?}");
        match &h[2] {
            RichMessage::TurnLedger { memory, .. } => {
                assert!(memory.narrative_only);
                assert_eq!(memory.attachments, 0);
                assert!(memory.tools.is_empty());
            }
            other => panic!("expected TurnLedger, got {other:?}"),
        }
    }

    #[test]
    fn remember_turn_reads_the_ledger_part_and_counts_images() {
        use crate::llm::RichMessage;
        let runner = TaskRunner::new_stub_echo();
        let mut ledger = crate::turn_ledger::TurnLedger::default();
        ledger.record(crate::turn_ledger::Action {
            tool: "read_file".into(),
            target: "/x/info.txt".into(),
            outcome: crate::turn_ledger::Outcome::Failed("EDEADLK".into()),
            excerpt: None,
        });
        let reply = settle("prose".into(), &ledger);
        let input = Message {
            role: "user".into(),
            parts: vec![
                MessagePart::Text { text: "read it".into() },
                MessagePart::Data {
                    mime_type: "image/png".into(),
                    data: serde_json::json!({ "base64": "QkFTRTY0" }),
                },
            ],
        };
        runner.remember_turn("k1", None, &input, &reply);
        let store = runner.conversations.lock().unwrap();
        let h = store.map.get("k1").expect("remembered");
        assert_eq!(h.len(), 3);
        // The image itself is not stored (as before); the fact of it is.
        assert!(matches!(&h[0], RichMessage::Text { role, content } if role == "user" && content == "read it"));
        assert!(matches!(&h[1], RichMessage::Text { role, content } if role == "agent" && content == "prose"));
        match &h[2] {
            RichMessage::TurnLedger { memory, .. } => {
                assert_eq!(memory.attachments, 1);
                assert!(!memory.narrative_only);
                assert_eq!(memory.tools[0].error.as_deref(), Some("EDEADLK"));
            }
            other => panic!("expected TurnLedger, got {other:?}"),
        }
    }

    #[test]
    fn a_malformed_ledger_part_falls_back_to_narrative_only() {
        use crate::llm::RichMessage;
        let runner = TaskRunner::new_stub_echo();
        let reply = Message {
            role: "agent".into(),
            parts: vec![
                MessagePart::Text { text: "prose".into() },
                MessagePart::Data {
                    mime_type: TURN_LEDGER_MIME.into(),
                    data: serde_json::json!({ "not": "a ledger" }),
                },
            ],
        };
        let input = Message {
            role: "user".into(),
            parts: vec![MessagePart::Text { text: "hi".into() }],
        };
        runner.remember_turn("k2", None, &input, &reply);
        let store = runner.conversations.lock().unwrap();
        let h = store.map.get("k2").expect("remembered");
        assert!(matches!(&h[2], RichMessage::TurnLedger { memory, .. } if memory.narrative_only));
    }
```

- [ ] **Step 3.4 — watch them fail.**

```
cargo nextest run -p mur-agent-runtime remembered_with_a_narrative_only reads_the_ledger_part malformed_ledger_part
```
Expected: the first fails with `assertion left == right: 2 vs 3`; the other two fail to compile (`TURN_LEDGER_MIME` is defined by Step 3.2 — if you did 3.2 first they fail on `len 2 != 3`).

- [ ] **Step 3.5 — implement.** Replace `fn remember_turn` (≈ line 688) with:

```rust
    /// Persist this turn into multi-turn memory keyed by `key` (this turn's id),
    /// so the next send — whose `context.task_id` equals `key` — recalls it.
    ///
    /// Stores the user text, the reply text, and a `TurnLedger` projected from
    /// the reply's ledger Data part (spec 2026-09-19-turn-ledger-memory). A
    /// pasted image is not stored, but its presence is (`attachments`). A reply
    /// with no readable ledger — single-call paths, stub backends — is
    /// remembered as `narrative_only`, because "ran nothing" is the fact the
    /// next turn most needs. Roles `user`/`agent` map to Anthropic
    /// `user`/`assistant`.
    fn remember_turn(&self, key: &str, ctx: Option<&str>, input: &Message, reply: &Message) {
        let attachments = image_count(input);
        let memory = match ledger_of(reply) {
            Some(l) => crate::turn_ledger::TurnMemory::project(&l, attachments),
            None => crate::turn_ledger::TurnMemory::empty(attachments),
        };
        let turn = u32::try_from(self.turn_counter.load(Ordering::Relaxed)).unwrap_or(u32::MAX);
        let mut store = self.conversations.lock().unwrap_or_else(|e| e.into_inner());
        let mut h = store.prior(ctx);
        h.push(crate::llm::RichMessage::Text {
            role: "user".into(),
            content: text_of(input),
        });
        h.push(crate::llm::RichMessage::Text {
            role: "agent".into(),
            content: text_of(reply),
        });
        h.push(crate::llm::RichMessage::TurnLedger { turn, memory });
        store.remember(key.to_string(), h);
    }
```

Add next to `fn text_of`:

```rust
/// The base64 payload of the first `image/*` Data part, if any — the one
/// predicate for "this input carries an image", shared by `user_message`
/// (which renders it) and `remember_turn` (which only counts it).
fn image_part(p: &MessagePart) -> Option<(String, String)> {
    match p {
        MessagePart::Data { mime_type, data } if mime_type.starts_with("image/") => data
            .get("base64")
            .and_then(|v| v.as_str())
            .map(|b64| (mime_type.clone(), b64.to_string())),
        _ => None,
    }
}

/// How many images the user attached to `input`.
fn image_count(input: &Message) -> u32 {
    input.parts.iter().filter(|p| image_part(p).is_some()).count() as u32
}

/// The turn ledger `settle` attached to `reply`, if present and readable.
fn ledger_of(reply: &Message) -> Option<crate::turn_ledger::TurnLedger> {
    reply.parts.iter().find_map(|p| match p {
        MessagePart::Data { mime_type, data } if mime_type == TURN_LEDGER_MIME => {
            serde_json::from_value(data.clone()).ok()
        }
        _ => None,
    })
}
```

In `fn user_message`, replace the `let image = input.parts.iter().find_map(|p| match p { … });` block with:

```rust
    let image = input.parts.iter().find_map(image_part);
```

- [ ] **Step 3.6 — fix the two existing tests that counted pairs.** `threads_multi_turn_chat_memory`: change `Some(2)` to `Some(3)` and `assert_eq!(t2.len(), 4, …)` to `assert_eq!(t2.len(), 6, "2 prior + 2 current turns × 3 = 6");` and update the comment string. Any other test asserting a stored length (grep `store.map.get(` and `prior(Some(` in the test module) gets the same ×3/2 adjustment; `conversation_survives_a_restart` and `history_is_trimmed_by_tokens_not_turn_count` build histories by hand and are untouched here (Task 4 rewrites the second).

- [ ] **Step 3.7 — run, lint, commit.**

```
cargo nextest run -p mur-agent-runtime task_runner
cargo clippy -p mur-agent-runtime --all-targets --no-deps -- -D warnings
cargo fmt -p mur-agent-runtime
```
Expected: all task_runner tests pass. Commit:

```
git add -A && git commit -m "feat(runtime): remember each turn with its ledger, narrative_only when nothing ran

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 4 — trim by whole turn; incident regression lock

**Interfaces — Consumes:** Task 3's triple layout.
**Interfaces — Produces:** `ConversationStore::remember` drops whole turns; `MAX_CONV_MESSAGES` doc updated.

- [ ] **Step 4.1 — rewrite the trim test (red).** Replace `history_is_trimmed_by_tokens_not_turn_count` with:

```rust
    /// #1200: the cap is a token budget, so many tiny turns are kept where two
    /// huge ones are not — and (2026-09-19) trimming removes whole turns, so a
    /// ledger never outlives the text it describes.
    #[test]
    fn history_is_trimmed_by_tokens_in_whole_turns() {
        use crate::llm::RichMessage;
        let msg = |role: &str, n: usize| RichMessage::Text {
            role: role.into(),
            content: "x".repeat(n),
        };
        let ledger = |turn: u32| RichMessage::TurnLedger {
            turn,
            memory: crate::turn_ledger::TurnMemory::empty(0),
        };
        const BUDGET: u64 = 900; // ≈ 3600 chars
        let mut store = ConversationStore {
            budget_tokens: BUDGET,
            ..Default::default()
        };

        // 20 turns × (20 + 20 chars + ~110-char ledger) ≈ 3000 chars: inside.
        let small: Vec<_> = (0..20)
            .flat_map(|i| [msg("user", 20), msg("agent", 20), ledger(i)])
            .collect();
        assert!(estimated_tokens(&small) <= BUDGET, "test setup exceeds budget");
        store.remember("small".into(), small);
        assert_eq!(store.prior(Some("small")).len(), 60, "trimmed by count, not tokens");

        // An oversized early turn is dropped as a unit — all three messages.
        let big = vec![
            msg("user", 4_000),
            msg("agent", 4_000),
            ledger(1),
            msg("user", 8),
            msg("agent", 8),
            ledger(2),
        ];
        assert!(estimated_tokens(&big) > BUDGET, "test setup fits the budget");
        store.remember("big".into(), big);
        let kept = store.prior(Some("big"));
        assert_eq!(kept.len(), 3, "oversized early turn was not dropped whole: {kept:?}");
        assert!(matches!(&kept[0], RichMessage::Text { role, .. } if role == "user"));
        assert!(matches!(&kept[2], RichMessage::TurnLedger { turn: 2, .. }));

        // Legacy pairs (files written before ledgers) still trim by turn.
        let legacy = vec![msg("user", 4_000), msg("agent", 4_000), msg("user", 8), msg("agent", 8)];
        store.remember("legacy".into(), legacy);
        let kept = store.prior(Some("legacy"));
        assert_eq!(kept.len(), 2);
        assert!(matches!(&kept[0], RichMessage::Text { role, .. } if role == "user"));
    }

    /// The newest turn is stored even when it alone exceeds the budget — the
    /// alternative is remembering nothing about the turn that just happened.
    #[test]
    fn the_newest_turn_is_never_trimmed_away() {
        use crate::llm::RichMessage;
        let mut store = ConversationStore { budget_tokens: 10, ..Default::default() };
        let only = vec![
            RichMessage::Text { role: "user".into(), content: "x".repeat(500) },
            RichMessage::Text { role: "agent".into(), content: "y".repeat(500) },
            RichMessage::TurnLedger { turn: 1, memory: crate::turn_ledger::TurnMemory::empty(0) },
        ];
        store.remember("only".into(), only);
        assert_eq!(store.prior(Some("only")).len(), 3);
    }
```

- [ ] **Step 4.2 — watch it fail.**

```
cargo nextest run -p mur-agent-runtime trimmed_by_tokens_in_whole_turns newest_turn_is_never
```
Expected: `history_is_trimmed_by_tokens_in_whole_turns` fails on the `big` case (`kept.len()` is 4, the pair-drain left the ledger orphaned).

- [ ] **Step 4.3 — implement whole-turn trimming.** Replace `fn remember` in `impl ConversationStore` with:

```rust
    /// Store `history` under `key`, trimming the oldest turns to the token
    /// budget and evicting the oldest conversation if over the cap.
    ///
    /// A turn is `[user, agent, ledger?]`; trimming drops whole turns so a
    /// ledger never outlives the text it describes and the history keeps
    /// starting on a `user` message (Anthropic requires that). The newest
    /// turn is always kept.
    fn remember(&mut self, key: String, mut history: Vec<crate::llm::RichMessage>) {
        while turn_count(&history) > 1 && estimated_tokens(&history) > self.budget_tokens {
            drop_oldest_turn(&mut history);
        }
        while history.len() > MAX_CONV_MESSAGES && turn_count(&history) > 1 {
            drop_oldest_turn(&mut history);
        }
        self.sweep_stale_files();
        self.persist(&key, &history);
        if self.map.insert(key.clone(), history).is_none() {
            self.order.push_back(key);
            while self.order.len() > MAX_CONVERSATIONS {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                    self.forget_file(&old);
                }
            }
        }
    }
```

Add as free functions next to `estimated_tokens`:

```rust
/// Does this message open a turn — a user-authored `Text`/`ImageText`?
fn opens_turn(m: &crate::llm::RichMessage) -> bool {
    use crate::llm::RichMessage as M;
    matches!(m, M::Text { role, .. } | M::ImageText { role, .. } if role == "user")
}

fn turn_count(history: &[crate::llm::RichMessage]) -> usize {
    history.iter().filter(|m| opens_turn(m)).count()
}

/// Remove the oldest turn: index 0 up to (not including) the next message
/// that opens a turn. On a history that does not start with a user message
/// (nothing today writes one) this still removes up to the next user turn.
fn drop_oldest_turn(history: &mut Vec<crate::llm::RichMessage>) {
    let end = history
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, m)| opens_turn(m))
        .map_or(history.len(), |(i, _)| i);
    history.drain(0..end);
}
```

Update the `MAX_CONV_MESSAGES` doc comment: replace the sentence beginning `MUST stay even:` through `(Anthropic requires that).` with `Applied in whole turns (see \`remember\`), so the history always starts on a \`user\` message (Anthropic requires that).`

- [ ] **Step 4.4 — watch it pass.**

```
cargo nextest run -p mur-agent-runtime trimmed_by_tokens_in_whole_turns newest_turn_is_never conversation_survives
```
Expected: 3 passed.

- [ ] **Step 4.5 — the incident regression lock (write, run — it passes against Task 3; it exists to stay green).** In the task_runner test module:

```rust
    /// 2026-09-18, channel 01a0b304: twenty-four text-only pairs of
    /// "short command → report claiming completion" and a reply produced by
    /// one model call with zero tool calls. Locks the mechanism, not the
    /// model: the next turn's message list must carry, immediately before
    /// the new user message, the runtime's record that the previous turn ran
    /// nothing. Whether the model then calls a tool is its business.
    #[test]
    fn the_turn_before_a_new_message_says_whether_it_ran_anything() {
        use crate::llm::RichMessage;
        let runner = TaskRunner::new_stub_echo();
        {
            let mut store = runner.conversations.lock().unwrap();
            let pairs: Vec<RichMessage> = (0..24)
                .flat_map(|i| {
                    [
                        RichMessage::Text { role: "user".into(), content: format!("continue {i}") },
                        RichMessage::Text {
                            role: "agent".into(),
                            content: format!("PR #{} 開好了，全綠。", 1400 + i),
                        },
                    ]
                })
                .collect();
            store.remember("prior".into(), pairs);
        }
        // The fabricating turn: prose, no tools.
        let input = Message {
            role: "user".into(),
            parts: vec![MessagePart::Text { text: "這疊往 main 推一格".into() }],
        };
        let reply = settle("推了一格：#1402 已 merged。".into(), &crate::turn_ledger::TurnLedger::default());
        runner.remember_turn("fab", Some("prior"), &input, &reply);

        let next = Message {
            role: "user".into(),
            parts: vec![MessagePart::Text { text: "真的？".into() }],
        };
        let seeded = runner.seed_history(Some("fab"), String::new(), &next);
        let n = seeded.len();
        assert!(matches!(&seeded[n - 1], RichMessage::Text { role, content } if role == "user" && content == "真的？"));
        match &seeded[n - 2] {
            RichMessage::TurnLedger { memory, .. } => {
                assert!(memory.narrative_only, "the empty turn must be on the record");
                assert_eq!(memory.attachments, 0);
                let rendered = crate::turn_ledger::render_memory(0, memory);
                assert!(rendered.contains("narrative_only: true"), "{rendered}");
            }
            other => panic!("expected the previous turn's ledger, got {other:?}"),
        }
    }
```

```
cargo nextest run -p mur-agent-runtime says_whether_it_ran_anything
```
Expected: 1 passed. Then, as a negative control, temporarily change `h.push(crate::llm::RichMessage::TurnLedger { turn, memory });` in `remember_turn` to a comment, re-run, confirm the test FAILS with `expected the previous turn's ledger`, and restore the line. Note the failing output in the commit message.

- [ ] **Step 4.6 — full crate run, lint, commit.**

```
RUST_MIN_STACK=33554432 cargo nextest run -p mur-agent-runtime
cargo clippy -p mur-agent-runtime --all-targets --no-deps -- -D warnings
cargo fmt -p mur-agent-runtime
cargo fmt --check
```
Expected: all pass; both exit 0. Commit:

```
git add -A && git commit -m "fix(runtime): trim conversation memory by whole turn; lock the 2026-09-18 incident

Negative control: with the TurnLedger push removed, the lock fails with
'expected the previous turn's ledger'.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 5 — workspace gate, live check, PR

- [ ] **Step 5.1 — workspace compiles with the new variant** (the variant is `pub`; `mur-core/src/cmd/agent_companion/preview.rs` constructs `RichMessage::Text` only, so no change is expected — verify):

```
cargo check --workspace --all-targets
cargo clippy --all --all-targets --no-deps -- -D warnings
```
Expected: exit 0 for both. If `mur-core` needs `MUR_WEB_DIST`, set it as `build.sh` does (`MUR_WEB_DIST=$HOME/Projects/mur-web/dist`).

- [ ] **Step 5.2 — live check on the concierge** (spec §7). Build and install (`./build.sh --install`), `mur agent restart mur`, then in `murmur`: one turn that runs a tool (`列出 ~/.mur/agents 有幾個目錄`) and one pure chat turn (`你好`). Then:

```
ls -t ~/.mur/agents/mur/conversations/*.json | head -2 | xargs -I{} sh -c 'echo {}; python3 -c "import json,sys; d=json.load(open(\"{}\")); print(json.dumps(d[-1], ensure_ascii=False)[:400])"'
```
Expected: the tool-using turn's file ends in `{"TurnLedger": {"turn": N, "memory": {"attachments": 0, "narrative_only": false, "tools": [{"tool": "bash", …}]}}}`; the chat turn's ends in `… "narrative_only": true, "tools": []`.

- [ ] **Step 5.3 — open the PR.** Base `main`, title `feat(runtime): remember each turn's tool ledger, not just its prose`. Body: link the spec, the incident (channel `01a0b304`, 2026-09-18 23:06Z), the negative-control line from Task 4, and the live-check output from Step 5.2. Ends with `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.

---

## Self-review

- **Spec coverage:** §3.1 → Task 1 (types, caps); §3.2 → Task 1 (`excerpt_for`, three rows, `cd` prefix only); §3.3 → Task 2; §4.1 → Task 3 Step 3.1; §4.2 (unconditional Data part, synthesis, `image_count` shared predicate, rejected alternative not built) → Task 3; §4.3 triple → Task 3; §4.4 rendering + `task_summary` skip → Task 2; §4.5 budget + whole-turn trim + no migration (legacy case in the trim test) → Task 4; §5 (malformed part, newest turn kept) → Tasks 3/4; §6 every listed test → Tasks 1–4; §7 → Task 5.
- **Placeholders:** none; every step shows code or an exact command with expected output.
- **Cross-task names:** `TurnMemory::{project, empty}`, `render_memory(turn, &memory)`, `MEMORY_OPEN`/`MEMORY_CLOSE`/`MEMORY_ROWS`, `RichMessage::TurnLedger { turn, memory }`, `TURN_LEDGER_MIME`, `image_part`/`image_count`/`ledger_of`, `opens_turn`/`turn_count`/`drop_oldest_turn` — used with the same spelling in every task.
