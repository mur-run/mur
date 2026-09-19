# Plan: unverified-claim settlement card

**Spec:** `docs/superpowers/specs/2026-09-19-unverified-claim-card-design.md` (read it first).
**Execution skill:** `mur-executing-plans` (one crate, three tasks, sequential).
**Branch:** implement on `fix/unverified-claim-card` branched from `origin/main` in a worktree under `.worktrees/`; tick this plan on `spec/unverified-claim-card`.

**Goal:** a turn that ran no tool yet names external state (SHA, PR number, GitHub state word, test tally, GitHub URL, diff) gets the settlement card with one muted `⚠ unverified` row, and its ledger Data part says `claims_external_state: true`.

**Architecture:** one structural predicate over the reply text (`external_state::claims_external_state`) evaluated in `settle()`; its result is stored on `TurnLedger`, `warrants_settlement()` gains the clause `actions.is_empty() && claims_external_state`, and `render()` prints the unverified row for that case. Nothing else changes: the card rides in the reply text to every consumer as today.

**Tech stack:** Rust 2024, `regex = "1"` (no lookaround — the SHA and `#` rules are hand scans), `serde`, `cargo nextest`. Crate: `mur-agent-runtime` only.

## Global Constraints (from the spec — every task includes these)

- The gate is structural, never lexical: no completion vocabulary in any language.
- `\b` and lookaround are not used for the SHA and `#` rules; a maximal hex run and a preceding-character check are. `main現在是1e0a4d40` must match.
- Nothing is refused, rewritten, retried, or re-prompted. `stop`, `iterations`, token counts, HITL state, memory: unchanged.
- A turn that ran tools is never affected: the new clause requires `actions.is_empty()`.
- `claims_external_state` is additive on `TurnLedger`: `#[serde(default, skip_serializing_if = "is_false")]`; old ledgers deserialise with `false`.
- Same `⚠` glyph as the existing "nothing ran" row, so the TUI paints it muted, not red.
- Commands: `cargo nextest run -p mur-agent-runtime <filter>`; `cargo clippy -p mur-agent-runtime --all-targets --no-deps -- -D warnings`; `cargo fmt -p mur-agent-runtime`. Run cargo with the Bash sandbox off (its jobserver FIFO hangs cmake build scripts). Read exit codes, not grep output.
- Commit messages end with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`.

## File structure

| File | Responsibility |
|---|---|
| `mur-agent-runtime/src/external_state.rs` (new) | `claims_external_state(text) -> bool` and its tests. Separate file: `turn_ledger.rs` is 884 lines, past the 800-line rule. |
| `mur-agent-runtime/src/lib.rs` | `pub mod external_state;` |
| `mur-agent-runtime/src/turn_ledger.rs` | re-export; `TurnLedger.claims_external_state`; `is_false`; `unverified_claim()`; the `warrants_settlement` clause; the `render` branch. Tests. |
| `mur-agent-runtime/src/task_runner.rs` | `settle()` sets the field. Regression lock + negative control. |
| `docs/superpowers/plans/2026-09-19-unverified-claim-card-plan.md` | This file. |

---

## Task 1 — `external_state.rs`: the predicate

**Interfaces — Produces:**

```rust
// mur-agent-runtime/src/external_state.rs
pub fn claims_external_state(text: &str) -> bool;
// re-exported as crate::turn_ledger::claims_external_state (Task 2)
```

- [x] **Step 1.1 — write the tests first (red).** Create `mur-agent-runtime/src/external_state.rs` with only the module doc and the test module:

```rust
//! Does a reply name external state — a SHA, a PR number, a test tally, a
//! diff — the shapes a tool leaves behind? Structural on purpose: no
//! completion vocabulary in any language, so a rephrased report does not
//! slip through and a well-worded answer is not caught. Mis-fires are
//! accepted (spec 2026-09-19-unverified-claim-card §2.3).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_signal_row_fires_on_its_own() {
        for (label, text) in [
            ("sha", "main is now at 1e0a4d40"),
            ("pr", "opened #1402"),
            ("state", "the PR is MERGED now"),
            ("tally", "3048 passed / 0 failed"),
            ("tally-result", "test result: ok. 12 passed"),
            ("url", "see https://github.com/mur-run/mur/pull/1402"),
            ("diff", "here:\ndiff --git a/x.rs b/x.rs\n"),
            ("hunk", "patch:\n@@ -1,3 +1,4 @@\n"),
            ("stat", "2 files changed, 10 insertions(+), 1 deletion(-)"),
        ] {
            assert!(claims_external_state(text), "{label}: {text:?}");
        }
    }

    #[test]
    fn a_sha_is_found_flush_against_cjk_text() {
        assert!(claims_external_state("main現在是1e0a4d40。"));
        assert!(claims_external_state("`main` 現在是 `1e0a4d40`"));
    }

    #[test]
    fn digits_alone_are_not_a_sha() {
        assert!(!claims_external_state("call 1234567 today"));
        assert!(!claims_external_state("Version 2.85.0"));
    }

    #[test]
    fn a_hex_run_longer_than_a_sha_is_not_a_sha() {
        let long = "1".to_string() + &"deadbeef00".repeat(5); // 51 hex chars
        assert!(!claims_external_state(&long));
    }

    #[test]
    fn a_hash_needs_two_digits_and_a_non_word_before_it() {
        assert!(!claims_external_state("#1"));
        assert!(!claims_external_state("abc#1402"));
        assert!(claims_external_state("#1402"));
        assert!(claims_external_state("（#1402）"));
    }

    #[test]
    fn state_words_must_be_upper_case_whole_words() {
        assert!(!claims_external_state("the branch was merged"));
        assert!(!claims_external_state("UNMERGEDX"));
        assert!(claims_external_state("state: MERGEABLE"));
    }

    #[test]
    fn diff_markers_must_start_a_line() {
        assert!(!claims_external_state("use --- as a separator"));
        assert!(!claims_external_state("email me @@ 5pm"));
        assert!(claims_external_state("\n--- a/x\n+++ b/x\n"));
    }

    #[test]
    fn plain_chat_and_explanations_do_not_fire() {
        assert!(!claims_external_state("你好"));
        assert!(!claims_external_state("哈囉，今天想折騰點什麼？"));
        assert!(!claims_external_state(
            "rebase 會把你的 commit 重新套到新的 base 上，衝突要逐個解。"
        ));
        assert!(!claims_external_state(""));
    }
}
```

- [x] **Step 1.2 — declare the module and watch it fail.** In `mur-agent-runtime/src/lib.rs` add `pub mod external_state;` directly after `pub mod turn_memory;`. Run:

```
cargo nextest run -p mur-agent-runtime external_state
```
Expected: compile error `cannot find function claims_external_state`.

- [x] **Step 1.3 — implement.** Insert between the module doc and `#[cfg(test)]`:

```rust
use regex::Regex;
use std::sync::OnceLock;

/// Length bounds of a git object id, abbreviated to full.
const SHA_MIN: usize = 7;
const SHA_MAX: usize = 40;
/// Digits a `#` reference needs. `#1` is a heading or a footnote; `#14` is a
/// pull request.
const HASH_MIN_DIGITS: usize = 2;

/// Rows that regexes can express without lookaround. `\b` is safe here: every
/// token is an ASCII word, so the CJK boundary problem does not apply.
fn patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            // GitHub's own state strings, as `gh … --json` prints them.
            r"\b(?:MERGED|MERGEABLE|CONFLICTING|UNSTABLE)\b",
            // A test runner's tally.
            r"\d+ passed|\d+ failed|test result:",
            // A GitHub object page.
            r"github\.com/\S+/(?:pull|commit|issues)/",
            // A diff or patch: markers that only mean something at line start.
            r"(?m)^(?:diff --git|@@|\+\+\+ |--- )",
            // `git diff --stat` / merge summary.
            r"\d+ files? changed|\d+ insertions?\(\+\)|\d+ deletions?\(-\)",
        ]
        .into_iter()
        .map(|p| Regex::new(p).expect("static pattern"))
        .collect()
    })
}

/// A maximal run of hex characters that is a plausible git object id: 7–40
/// long, all lowercase, at least one letter. Maximal-run scanning is the
/// lookaround-free form of `(?<![0-9a-fA-F])[0-9a-f]{7,40}(?![0-9a-fA-F])`,
/// and unlike `\b` it works flush against CJK text.
fn has_sha(text: &str) -> bool {
    fn plausible(len: usize, lowercase: bool, letter: bool) -> bool {
        lowercase && letter && (SHA_MIN..=SHA_MAX).contains(&len)
    }
    let mut run_len = 0usize;
    let mut run_ok = true; // all lowercase hex so far
    let mut run_has_letter = false;
    for c in text.chars() {
        if c.is_ascii_hexdigit() {
            run_len += 1;
            if c.is_ascii_uppercase() {
                run_ok = false;
            }
            if c.is_ascii_alphabetic() {
                run_has_letter = true;
            }
        } else {
            if plausible(run_len, run_ok, run_has_letter) {
                return true;
            }
            run_len = 0;
            run_ok = true;
            run_has_letter = false;
        }
    }
    plausible(run_len, run_ok, run_has_letter)
}

/// `#` followed by at least `HASH_MIN_DIGITS` digits, not glued to a word
/// character on the left (`abc#1402` is not a reference; `（#1402）` is).
fn has_hash_ref(text: &str) -> bool {
    let mut prev: Option<char> = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '#' && !prev.is_some_and(|p| p.is_ascii_alphanumeric() || p == '_') {
            let mut digits = 0usize;
            while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                chars.next();
                digits += 1;
            }
            if digits >= HASH_MIN_DIGITS {
                return true;
            }
            prev = Some('0'); // we consumed digits; next char cannot be flush
            continue;
        }
        prev = Some(c);
    }
    false
}

/// Does `text` carry external-state evidence (spec §2.1)? One pass per rule.
pub fn claims_external_state(text: &str) -> bool {
    has_sha(text) || has_hash_ref(text) || patterns().iter().any(|re| re.is_match(text))
}
```

- [x] **Step 1.4 — watch them pass, lint, commit.**

```
cargo nextest run -p mur-agent-runtime external_state
cargo clippy -p mur-agent-runtime --all-targets --no-deps -- -D warnings
cargo fmt -p mur-agent-runtime
```
Expected: 8 passed; clippy exit 0. Commit:

```
git add -A && git commit -m "feat(runtime): claims_external_state — the shapes a tool leaves in a reply

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 2 — ledger field, settlement clause, card row

**Interfaces — Consumes:** `crate::external_state::claims_external_state`.
**Interfaces — Produces:**

```rust
// turn_ledger.rs
pub use crate::external_state::claims_external_state;
pub struct TurnLedger { …, pub claims_external_state: bool }   // serde default, skip if false
impl TurnLedger { pub fn unverified_claim(&self) -> bool }     // actions.is_empty() && claims_external_state
pub const UNVERIFIED_ROW: &str =
    "  ⚠ unverified  no tool ran this turn — external state claims above were not checked\n";
```

- [x] **Step 2.1 — write the tests (red).** In `turn_ledger.rs` `mod tests`, after `settlement_triggers_on_change_failure_or_dirty_stop`:

```rust
    #[test]
    fn a_zero_tool_turn_that_names_external_state_warrants_settlement() {
        let claimed = TurnLedger {
            claims_external_state: true,
            ..Default::default()
        };
        assert!(claimed.unverified_claim());
        assert!(claimed.warrants_settlement());

        // Same claim, but a tool ran: the other clauses decide, and a single
        // successful read decides "no card" exactly as before.
        let mut read = TurnLedger {
            claims_external_state: true,
            ..Default::default()
        };
        read.record(act("read_file", "README.md", Outcome::Ok));
        assert!(!read.unverified_claim());
        assert!(!read.warrants_settlement());

        // No claim, nothing ran: still a pure question, still no card.
        assert!(!TurnLedger::default().unverified_claim());
        assert!(!TurnLedger::default().warrants_settlement());
    }

    #[test]
    fn render_prints_the_unverified_row_for_a_zero_tool_claim() {
        let l = TurnLedger {
            claims_external_state: true,
            ..Default::default()
        };
        let card = render(&l);
        assert!(card.contains(UNVERIFIED_ROW.trim_end()), "{card}");
        assert!(!card.contains("nothing ran"), "{card}");
        assert!(card.contains("⚠ unverified"), "{card}");
        assert!(!card.contains("✔"), "{card}");
    }

    #[test]
    fn the_claim_flag_is_additive_on_the_wire() {
        // A ledger written before the field existed.
        let old = r#"{"actions":[],"stop":"end_turn","iterations":0,"input_tokens":0,"output_tokens":0}"#;
        let l: TurnLedger = serde_json::from_str(old).unwrap();
        assert!(!l.claims_external_state);
        // `false` is not written; `true` round-trips.
        let s = serde_json::to_string(&TurnLedger::default()).unwrap();
        assert!(!s.contains("claims_external_state"), "{s}");
        let flagged = TurnLedger {
            claims_external_state: true,
            ..Default::default()
        };
        let s = serde_json::to_string(&flagged).unwrap();
        assert!(s.contains(r#""claims_external_state":true"#), "{s}");
        let back: TurnLedger = serde_json::from_str(&s).unwrap();
        assert!(back.claims_external_state);
    }
```

- [x] **Step 2.2 — watch them fail.**

```
cargo nextest run -p mur-agent-runtime names_external_state_warrants unverified_row claim_flag_is_additive
```
Expected: compile error `no field claims_external_state`.

- [x] **Step 2.3 — implement.** Four edits in `turn_ledger.rs`.

(a) After the existing `pub use crate::turn_memory::{ … };` re-export add:

```rust
pub use crate::external_state::claims_external_state;
```

(b) Replace the `TurnLedger` struct and its `Default` impl:

```rust
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
    /// a test tally, a diff (`claims_external_state`). Set by `settle`;
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
```

(c) Replace `warrants_settlement` (keep its doc comment, append one sentence) and add `unverified_claim` right after it inside the same `impl`:

```rust
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
```

(d) In `render`, replace the `if verified.is_empty() { … }` arm's body. Add the constant above `pub fn render`:

```rust
/// The verified row for a turn that ran nothing yet named external state.
/// Same `⚠` as the "nothing ran" row, so the TUI paints it muted: a warning
/// about missing evidence, not a failure.
pub const UNVERIFIED_ROW: &str =
    "  ⚠ unverified  no tool ran this turn — external state claims above were not checked\n";
```

and change the arm to:

```rust
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
```

- [x] **Step 2.4 — `settle()` sets the flag.** In `task_runner.rs` replace the body of `fn settle`:

```rust
fn settle(text: String, ledger: &crate::turn_ledger::TurnLedger) -> Message {
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
```

- [x] **Step 2.5 — regression lock and negative control (write, run — green against 2.3/2.4).** In the `task_runner.rs` test module, after `the_turn_before_a_new_message_says_whether_it_ran_anything`:

```rust
    /// 2026-09-18, channel 01a0b304: the reply below came from one model call
    /// with zero tool calls. Second line of defence — the user sees the card.
    #[test]
    fn a_zero_tool_report_of_external_state_carries_the_unverified_card() {
        let reply = settle(
            "推了一格：**#1402 已 merged**，`main` 現在是 `1e0a4d40`。剩下六個全部 rebase 到新 `main`、force-push 完成。".into(),
            &crate::turn_ledger::TurnLedger::default(),
        );
        let text = text_of(&reply);
        assert!(text.contains("─ settlement ─"), "{text}");
        assert!(text.contains("⚠ unverified"), "{text}");
        assert!(!text.contains("nothing ran"), "{text}");
        let ledger = ledger_of(&reply).expect("ledger part");
        assert!(ledger.claims_external_state);
        assert!(ledger.unverified_claim());
    }

    /// Negative control: a pure chat turn with the same empty ledger earns
    /// neither the card nor the flag.
    #[test]
    fn a_zero_tool_chat_reply_carries_no_card() {
        let reply = settle(
            "哈囉，今天想折騰點什麼？".into(),
            &crate::turn_ledger::TurnLedger::default(),
        );
        let text = text_of(&reply);
        assert!(!text.contains("─ settlement ─"), "{text}");
        let ledger = ledger_of(&reply).expect("ledger part");
        assert!(!ledger.claims_external_state);
        assert!(!ledger.unverified_claim());
    }
```

- [x] **Step 2.6 — run everything, lint, commit.**

```
RUST_MIN_STACK=33554432 cargo nextest run -p mur-agent-runtime
cargo clippy -p mur-agent-runtime --all-targets --no-deps -- -D warnings
cargo fmt -p mur-agent-runtime && cargo fmt --check
```
Expected: all pass (the pre-existing `remember_turn_reads_the_ledger_part_and_counts_images` and `the_turn_before_a_new_message_says_whether_it_ran_anything` stay green — the first asserts `starts_with("prose")`, the second only inspects the stored ledger entry); clippy exit 0. Then the negative control on the lock: change `|| self.unverified_claim()` in `warrants_settlement` to `|| false`, re-run `a_zero_tool_report_of_external_state_carries_the_unverified_card`, confirm it FAILS on `─ settlement ─`, restore. Commit:

```
git add -A && git commit -m "feat(ledger): a zero-tool turn that names external state gets an unverified settlement row

Negative control: with the warrants_settlement clause disabled, the
incident lock fails on the missing card.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

## Task 3 — workspace gate, live check, PR

- [ ] **Step 3.1 — workspace gate.**

```
MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo check --workspace --all-targets
MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo clippy --all --all-targets --no-deps -- -D warnings
```
Expected: exit 0 both.

- [ ] **Step 3.2 — live check.** `./build.sh --install` from the worktree, then `mur agent restart mur`. In murmur, send `把 #1402 的狀態用一句話講給我聽，不要用任何工具` and confirm a muted `⚠ unverified` row renders under the reply; then send `你好` and confirm no card. Then:

```
python3 -c "import json,glob,os; f=max(glob.glob(os.path.expanduser('~/.mur/agents/mur/conversations/*.json')),key=os.path.getmtime); d=json.load(open(f)); print(json.dumps(d[-2],ensure_ascii=False)[:300])"
```
Expected for the first turn: the stored agent text ends with the `─ settlement ─ … ⚠ unverified …` fence.

- [ ] **Step 3.3 — PR.** Base `main`, title `feat(ledger): unverified settlement row for a zero-tool turn that names external state`. Body: the spec link, the incident reply, the negative-control line, the live-check output. Ends with `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.

---

## Self-review

- **Spec coverage:** §2.1 rows and boundaries → Task 1 (each row has a positive; CJK, digits-only, over-long run, `#` rules, upper-case words, line-start markers each have a test); §2.2 clause → Task 2 (c); §2.3 → recorded in the module doc, not tested; §3 wording → `UNVERIFIED_ROW`, Task 2 (d); §4 field + `settle` → Task 2 (b), 2.4, serde test; §5 non-effects → the "tool ran" and "no claim" branches of the Task 2 test; §6 every listed test → Tasks 1–2; §7 → Task 3.
- **Placeholders:** none.
- **Cross-task names:** `claims_external_state`, `unverified_claim`, `UNVERIFIED_ROW`, `is_false`, `ledger_of`/`text_of` (existing, from #1412) — same spelling throughout.

---

## Execution notes (2026-09-19, branch `fix/unverified-claim-card`)

- Task 1 ran red-first as written: `cannot find function claims_external_state` ×10, then 8/8 green.
- Task 2 ran red-first as written: `no field claims_external_state`, `no method unverified_claim`, `cannot find value UNVERIFIED_ROW`, then the full crate at 1227 passed / 0 failed.
- Negative control on the incident lock: with `|| self.unverified_claim()` replaced by `|| false`, `a_zero_tool_report_of_external_state_carries_the_unverified_card` failed at `task_runner.rs:3639` (the missing `─ settlement ─`); restored, it passes alongside `a_zero_tool_chat_reply_carries_no_card`.
- No deviation from the plan's code. The sandboxed Bash tool still hangs cargo's cmake build scripts, so every cargo run had the sandbox off.
