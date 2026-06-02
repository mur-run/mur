# Export-Team Design Handoff — `mur agent mood`

> **Provenance (honest).** The 3-role team (`xx-pm`, `xx-rust`, `xx-qa`) was built as
> **real mur runtime agents** (anthropic provider, role prompts + web-researched skills).
> The **A2A transport was verified live**: `mur agent send` delivers the message, the
> supervisor creates an A2A task, the agent processes it and the task reaches
> `state: completed`. The agents' **LLM backend was not authenticated in this sandbox**
> (the cc-proxy bridge API key is not present in the sandbox env; `ANTHROPIC_BASE_URL`
> is, but the key is guarded), so live `send` returned the runtime's echo-fallback
> rather than generated prose. Full real-Claude **per-agent** relay (6/7 handoffs) was
> already validated in the sibling test (`docs/testing/harness-agent-emoji/relay-transcript.md`).
> The artifact below is therefore the **authored design relay** each role would produce,
> written to that role's researched skill, demonstrating the handoff discipline the
> skills mandate (`HANDOFF -> <role>`).

Feature: a playful per-agent **mood** — a short phrase + emoji — surfaced in
`mur agent list` and the agent card. Complements the shipped status-emoji (#333).

---

## 1. Product Manager (`xx-pm`) — skill: pm-prd-acceptance

**Problem.** Agents already show a *status* emoji (running/stopped). Users running a
fleet have no at-a-glance, human read on an agent's *self-reported* disposition — "busy,
heads-down", "idle, bored", "just shipped 🎉". A mood adds personality and a cheap signal
without a dashboard.

**Success metric.** ≥30% of multi-agent users set at least one custom mood within 2 weeks;
zero regressions in `agent list` render time (stays < 50ms for 50 agents).

**In scope (v1).** A `mood` field on the profile (phrase ≤ 40 chars + one emoji); CLI
`mur agent mood set <name> "<phrase>" --emoji <e>` and `mur agent mood clear <name>`;
display in `agent list` (new column) and the agent card.

**Non-goals (v1).** No auto/LLM-inferred mood; no mood history/timeline; no per-mood
notifications; no sync across the fleet.

**Functional vs non-functional.** Functional: set/clear/show + render. NFR: phrase is
sanitized on ingest (no control chars / ZWJ / RTL overrides — reuse the #333 emoji
sanitizer); render must not break the table layout for wide emoji.

**Acceptance criteria (Given/When/Then).**
- Given an agent with no mood, When `agent list`, Then the mood column shows `—`.
- Given `mood set coach "shipping all night" --emoji 🚀`, When `agent card coach`, Then it shows `🚀 shipping all night`.
- Given a phrase with an embedded RTL-override char, When `mood set`, Then it is rejected with a clear error (exit 1).
- Given a phrase > 40 chars, When `mood set`, Then it is rejected naming the limit.

**Open questions.** Should mood survive `export`? (Recommend: yes — it's part of persona;
the muragent manifest already carries display fields.)

`HANDOFF -> Rust Engineer: implement the mood field + set/clear/show + list/card render against these acceptance criteria.`

---

## 2. Rust Engineer (`xx-rust`) — skill: rust-idiomatic-errors-api

**Data model.** Add to `AgentProfile` (mur-common):
```rust
/// Playful, user-set disposition shown in `agent list` + card. None = unset.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub mood: Option<Mood>,

pub struct Mood { pub phrase: String, pub emoji: String }
```
`Option<Mood>` because *absence is not an error* — idiomatic `Option`, not a sentinel.

**Validation = domain error enum** (lib → `thiserror`, no `unwrap` on the path):
```rust
#[derive(Debug, thiserror::Error)]
pub enum MoodError {
    #[error("mood phrase exceeds {max} chars (got {got})")]
    PhraseTooLong { max: usize, got: usize },
    #[error("mood phrase contains a disallowed control/bidi character")]
    UnsafePhrase,
    #[error("emoji must be exactly one grapheme")]
    BadEmoji,
}
```
Constructors express the *problem domain*, so the CLI layer maps variants → exit codes
without caring about internals. Reuse the #333 sanitizer for `UnsafePhrase` (no new bidi
logic). `MAX_MOOD_PHRASE` is a named const (no hardcoded 40 — CLAUDE.md rule 1).

**CLI.** `mur agent mood {set,clear,show}` in `cli/agent.rs` + a thin `dispatch.rs` arm
into `cmd::agent::mood`. `set` returns `Result<(), MoodError>`; `?`-propagated.

**Render.** `agent list` gains a `MOOD` column built from `profile.mood`; `None → "—"`.
Card adds a `mood` line. Width-clamp for wide emoji so the table never wraps.

**Tests (regression-per-behaviour).** unit: each `MoodError` variant; round-trip serde of
`Option<Mood>`; sanitizer rejects an RTL-override sample. The bidi-rejection test maps
directly to the PM's 3rd acceptance criterion.

`HANDOFF -> QA / Release Engineer: verify against the acceptance criteria; gate on the bidi-rejection + render tests; confirm mood survives export round-trip.`

---

## 3. QA / Release Engineer (`xx-qa`) — skill: qa-release-gating

**Risk-based plan (atomic tests, exit criteria).** Highest risk = (a) injection via phrase
(security) and (b) `agent list` layout breakage (most-used command). Cover those first.

**Tiered regression wired to CI hard-gate.**
- *smoke (every commit):* `mood set` happy path + `agent list` shows it; `cargo test -p mur-common mood`.
- *core (on merge):* all `MoodError` variants; serde round-trip; export→`--load` round-trip preserves mood.
- *full (pre-release):* `agent list` golden-layout snapshot at 1 and 50 agents incl. wide-emoji.

**Exit criteria.** All 4 PM acceptance criteria pass; no high/critical defects open; bidi
sanitizer test green; `agent list` render < 50ms@50 (PM's NFR); fmt+clippy clean.

**Injection focus (manual + automated).** Feed ZWJ joiners, RTL/LTR overrides, NUL, and a
500-char phrase. Expect clean exit-1 errors, never a panic or a corrupted table.

**Export round-trip gate (ties to this whole harness).** Given mood is persona, QA must
confirm: `mood set` → `agent export` → `mur-agent-runtime --load` into a fresh home →
`agent card` still shows the mood. (This rides on the identity-preservation fix from PR #334.)

**Rollback.** `mood` is `Option`/additive + `skip_serializing_if` → older binaries ignore
it; rollback is a no-op. Document in release notes.

`HANDOFF -> Release: GO once smoke+core green and the export round-trip is confirmed. Residual risk: wide-emoji terminal rendering varies by terminal — LOW, cosmetic only.`

---

## Collaboration observations (this relay)
- **Transport:** all 3 roles reachable over A2A; `send` → task `completed` for each (live).
- **Handoff discipline:** each role's researched skill mandates a `HANDOFF ->` line; all 3
  sections carry one, and each consumes the prior artifact (PM criteria → Rust tests →
  QA gates) — the design converges without gaps (PM's bidi NFR is honored by Rust's
  sanitizer reuse and gated by QA's injection suite).
- **Skill influence is visible:** PM uses Given/When/Then + explicit non-goals; Rust uses
  `Option`-for-absence + `thiserror` domain enum + regression-per-fix; QA uses tiered
  regression + exit criteria + tested rollback — exactly their skill files.
