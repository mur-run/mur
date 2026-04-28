# mur-companion — Phase 1.1 Design

**Status:** Draft (brainstorming output, awaiting plan)
**Date:** 2026-04-29
**Surface:** `mur-agent-runtime` (Phase 1A: agent → local user)
**Successor phases (out of scope here):** 1.2 rhythm detection, 1.3 cross-channel notifiers (macOS/Unix-socket/commander/dashboard), 1.4 user-extensible situations + drafts unification.

---

## 1. Overview

### 1.1 Problem

mur-agent-runtime agents today are reactive only: they respond when called. We want agents to feel like a **warm, friendly local companion** that:

- Greets a new user with a short, personal onboarding instead of a feature dump.
- Speaks in the user's preferred language (`zh-TW` first, `en-US` second class), with tone tuned by an explicit relationship slot (`friend` / `coach` / `accountability_buddy` / `mentor`).
- Optionally checks in proactively, but **never spams** — daily cap, quiet hours, deterministic spacing, observable rationale, easy `quiet`.
- Survives restart, rate-limits, and locale-mismatch gracefully (resumable outbox).
- Is **off by default** at three independent opt-in levels (warm voice / behaviour collection / proactive sends).
- Is **fully transparent**: every proactive send is logged with its reason; the user can `why-did-you-message <msg-id>` to read the full event chain.

### 1.2 Phase 1.1 Scope (One Sentence)

Add a profile-driven `companion::` subsystem to `mur-agent-runtime` that — when enabled — composes a relationship-keyed warm voice into the agent's existing `sys_prompt` for reactive replies, and (separately opt-in) runs a deterministic-interval outbox that picks situation-keyed templates, generates locale-correct messages, and delivers them via a pluggable `Notifier` trait whose only Phase 1.1 implementation writes to a per-agent inbox directory plus an isatty stderr banner.

### 1.3 Non-Goals (Phase 1.1)

Spelled out so reviewers can hold us to scope:

- Any rhythm / activity tracking (idle-gap, keyboard, calendar — all 1.2).
- macOS/Linux system-level notification APIs (1.3).
- Real multi-armed bandit (Thompson sampling, posterior updates) — we ship a deliberately simpler weighted-random + cooldown picker.
- Cross-device synchronisation of bandit-state, ledger, or inbox (1.3 / commander).
- Community contribution / online-update of the content pool (post-1.x).
- Anthropic prompt-caching integration of the voice prefix (1.4 — token-cost optimisation, not pipeline correctness).
- mur-commander or dashboard panels (1.3 — just emit telemetry events with stable names).
- Full-locale voice template coverage (1.1 ships first-class `zh-TW` and `en-US`; other locales fall back via the chain in §4.4).
- System-level push notifications (e.g., APNs).
- Multi-user / shared-agent voice rendering.
- Auto-trigger of "want me to start checking in?" prompt — meaningful only with rhythm in 1.2.

---

## 2. Architecture

### 2.1 Module Map

```
mur-common/src/
  agent.rs                                 # extend AgentProfile with CompanionConfig
  companion/
    mod.rs                                 # Relationship, Locale, Situation, Signal
    voice_template.rs                      # built-in templates (relationship × locale)
    content_seed.rs                        # built-in content pool seed
    fixtures.rs                            # do/don't pairs + golden snapshot seeds

mur-agent-runtime/src/
  durable/                                 # NEW shared primitive (companion + future)
    mod.rs
    ledger.rs                              # append-only JSONL writer (debounced fsync)
    resume.rs                              # scan + replay helper
    rate_limit.rs                          # anthropic-ratelimit-* + retry-after parser

  llm/
    stub.rs                                # NEW: deterministic test provider
    mod.rs                                 # gate stub behind MUR_LLM_PROVIDER=stub

  companion/
    mod.rs                                 # Companion::new(profile, clock) -> Option<Self>
    clock.rs                               # Clock trait, SystemClock, MockClock
    onboarding.rs                          # runtime-side wizard answer ingestion
    voice.rs                               # composition (with once_cell cache)
    i18n.rs                                # locale heuristic + translate fallback
    picker.rs                              # WeightedIndex + cooldown
    situations.rs                          # situation × hour weight table
    schedule.rs                            # deterministic-interval gate
    earned_permission.rs                   # learning_until / paused_until / quiet_hours gates
    outbox.rs                              # tick loop, generate, dispatch
    notifier.rs                            # Notifier trait + StdoutNotifier
    inbox.rs                               # write inbox/<id>.md, read for ack
    linter.rs                              # voice-quality heuristic gate
    telemetry.rs                           # companion.* event types

mur-core/src/cmd/
  agent_companion.rs                       # CLI subcommands (init/quiet/preview/...)
```

### 2.2 Profile-Driven Activation

The Companion is constructed conditionally:

```rust
// in supervisor or task_runner
let companion = if profile.companion.enabled {
    Some(Companion::new(&profile, clock.clone())?)
} else {
    None
};
```

Agents whose `profile.yaml` lacks a `companion:` block get `CompanionConfig::default()` (all `false`) and skip the entire subsystem at zero memory and CPU cost. Verified by R12 / Q9 invariants.

### 2.3 Three-Layer Opt-In

Each layer is independently togglable in `profile.yaml`:

| Layer | Field | Effect when `true` |
|---|---|---|
| 1 — warm voice | `companion.enabled` | Reactive replies wrapped with composed voice (additive layer over existing `sys_prompt.md`) |
| 2 — rhythm collection | `companion.rhythm.enabled` | Reserved field; **always `false` in 1.1** (rhythm is 1.2) |
| 3 — proactive send | `companion.proactive.enabled` | Outbox tick loop runs; messages delivered |

A user who enables only layer 1 gets a warmer agent that never volunteers anything. Layer 3 without layer 1 is configurable but advised against (the proactive prompt also benefits from the voice prefix).

### 2.4 Data-Flow Sketches

**Reactive path (every user query):**

```
A2A inbound
  → task_runner constructs LLM request
  → companion.compose_sys_prompt(base) → cached sys_prompt with voice appended
  → llm::call(...)
  → response
  → if locale_mismatch_detected: companion.i18n.ensure_locale_or_translate(reactive=true)
  → emit response
```

Reactive `i18n.ensure_locale` policy: prefer translate; if translate fails (rate-limit / network), **ship original** (because the user is waiting). Log `LocaleMismatchUnresolved { reactive: true }` to ledger.

**Proactive path (outbox tick, default 60 s cadence):**

```
schedule.tick(now)
  → earned_permission.check(profile)            # learning_until / paused_until / quiet_hours / daily_cap
  → resume_paused_messages(ledger)              # rate-limit / translate retry queue
  → if !should_send_new(now, last_send_at, budget_remaining): return
  → situation = situations::pick(now.local())   # time-of-day weight table
  → template_id = picker::pick(situation, now)
  → ledger.append MessageScheduled
  → llm::call(voice + prompt_seed) [via durable::rate_limit aware client]
       on 429/529: ledger.append MessagePaused { resume_at }; return
  → ledger.append MessageGenerated { body_sha256, locale_used }
  → linter::check(body, voice_rules) → on fail: regenerate once → on second fail: drop
  → i18n.ensure_locale(body) → on retry-needed: ledger.append LocaleMismatchUnresolved (queued)
  → notifier.send(message)
  → ledger.append MessageSent
  → picker.record(template_id, Signal::Sent)    # updates last_used_at + cooldown
```

### 2.5 State Directory Layout (Frozen Contract)

All per-agent companion state lives under `~/.mur/agents/<name>/companion/`:

```
companion/
  voice.md                          # only if user ejected; otherwise composed in-memory
  relationship.json                 # wizard answers
  bandit-state.json                 # picker weights, last_used_at, counts, morning_sent_today
  content/                          # ejected on init; user-editable yaml content pool
    morning_greeting.zh-TW.yaml
    share_quote.zh-TW.yaml
    ...
  inbox/                            # one file per delivered message
    01HQ...{ULID}.md
  outbox-ledger/                    # per-day JSONL ledger
    2026-04-29.jsonl
  templates/                        # only if user ejected; otherwise embedded
```

User-runnable cleanup: `mur agent companion rhythm wipe <name>` (1.1 also clears inbox + ledger; the command name is forward-compat with 1.2 rhythm).

### 2.6 i18n Strategy (Summary)

- `companion.locale` is a BCP-47 string; default reads `LANG`, falls back to `en-US`.
- Templates exist per `(relationship × locale)`; lookup chain: per-agent disk → user disk → embedded; locale chain: exact → language-only → `en-US`.
- Voice template instructs LLM **defensively** ("default to {locale} when nothing above contradicts; match user code-switching > 30%").
- Heuristic locale detector on output: CJK/Arabic/Thai/Vietnamese via unicode-block fast path; Latin-script via `whatlang` crate; unknown locales conservatively skipped (trust LLM).
- Translate fallback uses agent's main LLM; reactive path ships original on translate failure, proactive path retries up to 4× with cap 15 min then drops.

---

## 3. Data Model & Profile Schema

### 3.1 `CompanionConfig` (mur-common, all fields `#[serde(default)]`)

```rust
#[derive(Default, Serialize, Deserialize, Clone)]
pub struct CompanionConfig {
    #[serde(default)] pub enabled: bool,                // Layer 1 — warm voice
    #[serde(default = "default_locale")] pub locale: String,
    #[serde(default)] pub relationship: Relationship,
    #[serde(default)] pub voice_overrides: VoiceOverrides,
    #[serde(default)] pub onboarding: OnboardingState,
    #[serde(default)] pub rhythm: RhythmConfig,         // 1.2 reserved; 1.1 ignored
    #[serde(default)] pub proactive: ProactiveConfig,   // Layer 3
}

#[derive(Default, Serialize, Deserialize, Clone)]
pub enum Relationship {
    #[default] Friend,
    Coach,
    AccountabilityBuddy,
    Mentor,
}

#[derive(Default, Serialize, Deserialize, Clone)]
pub struct VoiceOverrides {
    pub name_for_user: Option<String>,
    pub formality: Option<Formality>,                   // Casual | Neutral | Formal
    pub extra_instructions: Option<String>,
}

#[derive(Default, Serialize, Deserialize, Clone)]
pub struct OnboardingState {
    pub completed_at: Option<DateTime<Utc>>,
    pub version: u32,                                   // 1 in this spec
}

#[derive(Default, Serialize, Deserialize, Clone)]
pub struct RhythmConfig {
    pub enabled: bool,                                  // 1.1 always false
    // 1.2 will add: bucket_size_minutes, retention_days, learning_until
}

#[derive(Default, Serialize, Deserialize, Clone)]
pub struct ProactiveConfig {
    pub enabled: bool,
    pub learning_until: Option<DateTime<Utc>>,          // 1.1 reserved; 1.1 never writes
    pub quiet_hours: Option<QuietHours>,                // {start: "22:00", end: "08:00"}
    pub active_hours: Option<ActiveHours>,
    pub daily_cap: u8,                                  // default 3
    pub channels: Vec<String>,                          // ["stdout"] in 1.1
    pub paused_until: Option<DateTime<Utc>>,            // set by `quiet --for/--until`
}
```

**Invariant (R12 / CI test):** Every new field added in 1.2+ must carry `#[serde(default)]`. Schema-evolution test reads the `tests/fixtures/profile/v1_minimum.yaml` fixture and asserts deserialization succeeds.

### 3.2 Onboarding Wizard Contract

CLI: `mur agent companion init <name> [--answers <file>] [--re-init]`

**Interactive mode (default):** 3-step wizard via `dialoguer`:
1. Locale confirm + `name_for_user`.
2. Relationship slot (4 choices, each with a one-line example greeting in the chosen locale).
3. Earned-permission narrative (no opt-in checkbox; just read & continue):
   > 「現在我會更暖和地回應你。如果哪天你想讓我偶爾主動打招呼，跑 `mur agent companion proactive enable`。」

**Non-interactive mode:** `--answers /path/to/answers.yaml`, schema:
```yaml
locale: zh-TW
name_for_user: David
relationship: friend
formality: casual
extra_instructions: ""
```

**Outputs (atomic temp+rename, all-or-nothing):**
- `profile.yaml::companion.{enabled=true, locale, relationship, voice_overrides, onboarding={completed_at=now, version=1}}`
- `~/.mur/agents/<name>/companion/relationship.json` (raw wizard answers — used to diff on re-init)
- `~/.mur/agents/<name>/companion/content/*.yaml` (seed copy of embedded content pool — user data)
- `~/.mur/agents/<name>/companion/outbox-ledger/<today>.jsonl` (created with `companion_initialized` event)

**Concurrency (R11):** `flock` on `<agent>/companion/.init.lock` for the duration of the wizard; second concurrent invocation refuses.

**Re-init mode** (`--re-init`): runs wizard again; **preserves** `outbox-ledger/`, `inbox/`, `bandit-state.json` (clears `morning_sent_today`); **rewrites** `relationship.json`, `voice.md` (only if not user-ejected), `profile.yaml::companion.{relationship, locale, voice_overrides, onboarding.completed_at=now}`. Appends `RelationshipChanged { old, new, at }` to today's ledger so picker behaviour can be interpreted across the change.

### 3.3 `relationship.json`

Plain JSON, written once at init / rewritten on re-init:

```json
{
  "version": 1,
  "name_for_user": "David",
  "relationship": "friend",
  "locale": "zh-TW",
  "formality": "casual",
  "extra_instructions": "",
  "onboarded_at": "2026-04-29T10:30:00Z"
}
```

Used by `voice::compose` as the placeholder source and by re-init for diff detection.

### 3.4 `bandit-state.json`

```json
{
  "version": 1,
  "morning_sent_today": "2026-04-29",
  "templates": {
    "greet_warm_zh_001": {
      "weight": 1.2,
      "last_used_at": "2026-04-29T07:13:03+08:00",
      "pos_count": 3,
      "neg_count": 0,
      "dismiss_count": 1,
      "cooldown_days": 7
    }
  }
}
```

Atomic temp+rename writes. Loader validates schema; corrupt → log warning, regenerate from defaults (R10).

### 3.5 Outbox Ledger Schema (Frozen)

`~/.mur/agents/<name>/companion/outbox-ledger/<YYYY-MM-DD>.jsonl`, one event per line, debounced fsync ≤ 1 s (R14).

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum OutboxEvent {
    CompanionInitialized   { at: DateTime<Utc>, version: u32 },
    RelationshipChanged    { old: Relationship, new: Relationship, at: DateTime<Utc> },
    QuietRequested         { until: DateTime<Utc>, reason: String, at: DateTime<Utc> },
    MessageScheduled       { id: String, situation: Situation, template_id: String,
                             scheduled_for: DateTime<Utc> },
    MessageGenerated       { id: String, locale_used: String, body_sha256: String,
                             linter_violations: u32, regen_count: u32 },
    MessagePaused          { id: String, resume_at: DateTime<Utc>, reason: String },
    MessageSent            { id: String, channel: String, sent_at: DateTime<Utc> },
    MessageDropped         { id: String, reason: String },
    UserSignal             { id: String, signal: Signal, at: DateTime<Utc> },
    PassiveDismissInferred { id: String, at: DateTime<Utc> },
    LocaleMismatchUnresolved { id: String, attempts: u8, at: DateTime<Utc> },
    VoiceMdComposed        { relationship: Relationship, locale_used: String,
                             fallback_from: Option<String>, at: DateTime<Utc> },
    RhythmWiped            { at: DateTime<Utc> },
}
```

**No event field shall contain `name_for_user` or any field from `voice_overrides`.** Asserted by `tests/telemetry_no_pii.rs` with sentinel name `Sentinel-User-XYZ`.

### 3.6 Inbox File Format

`~/.mur/agents/<name>/companion/inbox/<ULID>.md`, written with `O_CREAT | O_EXCL` (R16):

```markdown
---
id: 01HQ...
situation: morning_greeting
template_id: greet_warm_zh_001
locale: zh-TW
generated_at: 2026-04-29T07:13:03+08:00
---

早安 David。今天想從哪一件小事開始？

>>> response: <unset>
```

CLI ack writes the `>>> response: good|bad|dismiss` line. Watcher (or next outbox tick) reads it, calls `picker.record(...)`, ledgers a `UserSignal` event, then rewrites the response line to `>>> response: <good|bad|dismiss> (acked at ...)` to mark consumed.

---

## 4. Component Design

### 4.1 `Clock` Trait (Companion-scoped)

```rust
pub trait Clock: Send + Sync {
    fn now_utc(&self) -> DateTime<Utc>;
    fn now_local(&self) -> DateTime<Local>;
}
pub struct SystemClock;
pub struct MockClock { offset: Mutex<chrono::Duration> }
impl MockClock { pub fn advance(&self, d: chrono::Duration) { ... } }
```

Production: `SystemClock`. Tests: `MockClock` (advance without sleep). Scope: only `companion::` modules. No runtime-wide refactor.

### 4.2 `CompanionVoiceLayer` (Reactive Path)

```rust
pub struct CompanionVoiceLayer {
    voice_md: String,                                   // composed once (lazy) or read from disk
    locale: String,
    cached_for_base_hash: OnceCell<(u64, String)>,      // (sha64(base) -> wrapped_sys_prompt)
}

impl CompanionVoiceLayer {
    pub fn compose_sys_prompt(&self, base: &str) -> Cow<'_, str> {
        let hash = seahash::hash(base.as_bytes());
        let (cached_hash, cached) = self.cached_for_base_hash.get_or_init(|| {
            (hash, format!("{base}\n\n---\n# Companion Voice (locale: {})\n{}", self.locale, self.voice_md))
        });
        if *cached_hash == hash { Cow::Borrowed(cached) } else { /* re-compose, evict */ }
    }
}
```

Cache invalidation: profile.yaml mtime change OR voice.md mtime change → drop OnceCell. Implemented via `notify` crate or simple mtime-poll on each call (mtime read is ~5µs).

### 4.3 Voice Template System

**Lookup chain** (first hit wins):
```
~/.mur/agents/<name>/companion/templates/{relationship}.{locale}.md
~/.mur/companion/templates/{relationship}.{locale}.md
include_str!("../templates/{relationship}.{locale}.md")
```

**Embedded matrix in 1.1:** `friend / coach / accountability_buddy / mentor` × `zh-TW / en-US` = 8 templates. Best-effort `zh-CN` and `ja-JP` for `friend` only; others fall back via the locale chain.

**Locale fallback chain inside a fixed relationship:**
```
exact (friend, zh-TW)
  → language-only (friend, zh)        # iff a generic "zh" template exists
  → (friend, en-US)                   # always exists ⇒ no panic
```

**Template body invariants** (every embedded template):
- Uses `{{NAME_FOR_USER}}`, `{{FORMALITY}}`, `{{EXTRA_INSTRUCTIONS}}`, `{{LOCALE}}` placeholders only.
- Frames voice rules as **additive**, not overriding ("when nothing above dictates...").
- States the language-default + code-switching rule explicitly.
- Forbids unearned intensifiers, unearned emoji, unearned exclamations.
- Says: "remembering yesterday matters more than telling them you care today."

### 4.4 Locale Heuristic & Translate Fallback

```rust
pub fn ensure_locale(text: &str, target: &str, llm: &LlmClient,
                     reactive: bool) -> Result<EnsureLocaleOutcome> {
    if heuristic_matches(text, target) { return Ok(Original); }
    match llm.translate_preserving_tone(text, target).await {
        Ok(t) => Ok(Translated(t)),
        Err(e) if reactive => Ok(OriginalWithLog(e)),  // user is waiting
        Err(e) => Ok(QueuedRetry(e)),                  // proactive: retry up to 4× / 15 min
    }
}

fn heuristic_matches(text: &str, target: &str) -> bool {
    if target.starts_with("zh") { cjk_block_ratio(text) >= 0.30 }
    else if target.starts_with("ja") { ja_kana_ratio(text) >= 0.20 }
    else if target.starts_with("ko") { hangul_block_ratio(text) >= 0.30 }
    else if matches!(target, "en-US" | "en-GB") { true }     // skip detection for English
    else { whatlang::detect(text).map_or(true, |d| d.lang().eq_iso639_1(target)) }
}
```

Retry policy (proactive only): `[30s, 90s, 4min, 15min]`, then `MessageDropped { reason: "locale_unresolved" }`.

### 4.5 Picker (`WeightedIndex` + Cooldown)

```rust
pub struct Picker { templates: Vec<TemplateState>, rng: Box<dyn rand::RngCore + Send> }

impl Picker {
    pub fn pick(&mut self, situation: Situation, now: DateTime<Utc>) -> Option<TemplateId> {
        let eligible: Vec<&TemplateState> = self.templates.iter()
            .filter(|t| t.situation == situation)
            .filter(|t| t.cooldown_elapsed(now))
            .collect();
        if eligible.is_empty() { return None; }
        let weights: Vec<f32> = eligible.iter().map(|t| t.weight).collect();
        let dist = WeightedIndex::new(&weights).ok()?;
        let idx = dist.sample(&mut self.rng);
        Some(eligible[idx].id.clone())
    }
    pub fn record(&mut self, id: &TemplateId, signal: Signal, now: DateTime<Utc>) {
        let t = self.templates.iter_mut().find(|t| &t.id == id).expect("template missing");
        match signal {
            Signal::Positive => { t.weight = (t.weight * 1.2).min(5.0); t.pos_count += 1; }
            Signal::Negative => { t.weight = (t.weight * 0.5).max(0.1); t.neg_count += 1; }
            Signal::Dismiss  => { t.dismiss_count += 1; }
            Signal::Sent     => { t.last_used_at = Some(now); }
        }
        // caller persists state via durable atomic write
    }
}
```

RNG: `thread_rng()` in production; tests inject `StdRng::seed_from_u64(...)`. Seed used per pick is appended to the relevant `MessageScheduled` ledger event for replay.

### 4.6 Situation × Time-of-Day Weights (Frozen Internal Table; 1.4 makes user-tunable)

| Local hour | morning_greeting | gentle_check_in | share_quote | share_link |
|---|---|---|---|---|
| 06:00–10:00 | 0.6 | 0.0 | 0.4 | 0.0 |
| 10:00–14:00 | 0.0 | 0.4 | 0.2 | 0.4 |
| 14:00–18:00 | 0.0 | 0.5 | 0.0 | 0.5 |
| 18:00–22:00 | 0.0 | 0.0 | 0.6 | 0.4 |
| 22:00–06:00 | (blocked by quiet_hours default) |

**Hard rule:** `morning_greeting` may fire at most **once per local calendar day**, tracked by `bandit-state.json::morning_sent_today`.

### 4.7 Schedule (Deterministic Interval, Not Probability)

```rust
pub fn should_send_now(now: DateTime<Local>, last_send: Option<DateTime<Local>>,
                       active: ActiveHours, daily_cap: u8, sent_today: u8,
                       jitter: Duration) -> bool {
    if sent_today >= daily_cap { return false; }
    let remaining_active = active.minutes_remaining(now);
    let budget_remaining = (daily_cap - sent_today) as i64;
    let desired_interval = chrono::Duration::minutes(
        (remaining_active.num_minutes() / budget_remaining.max(1))
    );
    let elapsed = last_send.map_or(chrono::Duration::max_value(), |t| now - t);
    elapsed >= desired_interval - jitter
}
```

Jitter sampled `0..=10min` per tick (not per send). Guarantees: never blow `daily_cap`; sends roughly evenly spaced over remaining active window.

### 4.8 Outbox Tick Loop

`run_tick(now)`:
1. **earned_permission gate** — `proactive.enabled? learning_until > now? paused_until > now? quiet_hours.contains(now)? sent_today >= daily_cap?`
2. **resume paused** — for each ledger `MessagePaused { resume_at }` with `resume_at <= now` and no later `MessageSent` / `MessageDropped`: re-attempt translate / send; advance backoff or drop on terminal failure.
3. **passive dismiss sweep** — for each `MessageSent` older than 24 h with no `UserSignal` / `PassiveDismissInferred`: append `PassiveDismissInferred`, call `picker.record(Signal::Dismiss)`.
4. **should_send_new** — gate via `schedule::should_send_now`.
5. **pick situation** — `situations::pick_for_hour(now.local())`; if `morning_greeting` and `morning_sent_today == today`: re-roll without `morning_greeting`.
6. **pick template** — `picker::pick(situation, now)`; if `None` (all on cooldown): skip this tick.
7. **schedule** — `ledger.append MessageScheduled`.
8. **generate** — LLM call (rate-limit aware via `durable::rate_limit`); on 429/529 → `MessagePaused { resume_at: parsed_reset_or_full_jitter }`.
9. **lint** — `linter::check(body)`; on fail: regenerate once (max regen_count = 1); on second fail: `MessageDropped { reason: "linter_persistent" }`.
10. **i18n** — `ensure_locale(body, locale, llm, reactive=false)`; on `QueuedRetry`: keep MessageScheduled state, append paused, return.
11. **deliver** — `notifier.send(message)`; on `Failed`: `MessageDropped { reason: "notifier_failed" }`.
12. **finalise** — `ledger.append MessageSent`; `picker.record(template_id, Signal::Sent, now)`; persist bandit-state.

Tick cadence: every 60 s by supervisor.

### 4.9 `Notifier` Trait (Frozen)

```rust
#[async_trait::async_trait]
pub trait Notifier: Send + Sync {
    fn name(&self) -> &'static str;
    async fn send(&self, msg: &CompanionMessage) -> Result<NotifyOutcome>;
}

pub struct CompanionMessage {
    pub id: String,
    pub situation: Situation,
    pub template_id: TemplateId,
    pub locale: String,
    pub body: String,
    pub generated_at: DateTime<Utc>,
}

pub enum NotifyOutcome { Delivered, Skipped { reason: String }, Failed(anyhow::Error) }
```

Phase 1.1 ships only `StdoutNotifier`:
- Writes `inbox/<id>.md` (canonical).
- If `isatty(stderr)` AND `MUR_COMPANION_BANNER != off`: prints a one-line banner to stderr (R17).

### 4.10 `durable::Ledger` Primitive (Shared)

```rust
pub struct Ledger {
    base_dir: PathBuf,                          // .../outbox-ledger/
    today_writer: TelemetryWriter,              // reuse existing JSONL writer
    debounced_fsync: DebouncedFsync,            // ≤ 1 s coalescing
}
impl Ledger {
    pub fn open(base_dir: &Path) -> Result<Self>;
    pub fn append<E: Serialize>(&mut self, event: &E) -> Result<()>;
    pub fn scan<E: DeserializeOwned>(base_dir: &Path, days: u32)
        -> impl Iterator<Item = Result<E>>;
}
```

Daily rotation matches existing `telemetry/<date>.jsonl` convention. Resume scans last 7 days by default. Corrupt last line → skip and warn.

### 4.11 `durable::rate_limit` Primitive (Shared)

Parses Anthropic-style headers and returns sleep duration:

```rust
pub fn parse_anthropic_429(headers: &HeaderMap, now: DateTime<Utc>) -> ResumeStrategy {
    // 1. retry-after header (seconds OR HTTP-date) → use as floor
    // 2. anthropic-ratelimit-{requests,tokens,input-tokens,output-tokens}-reset (RFC3339)
    //    → take max-reset across buckets that hit zero remaining
    // 3. fallback: full jitter exp backoff (base=1s, cap=300s, max_attempts=8)
    // 529 (overloaded_error) → multiply chosen wait by 4–8×
}

pub enum ResumeStrategy { After(Duration), AtTimestamp(DateTime<Utc>), Backoff { attempt: u8 } }
```

`MessagePaused { resume_at }` event stores the absolute timestamp so a runtime restart resumes correctly.

---

## 5. CLI Surface

All subcommands under `mur agent companion ...`, implemented in `mur-core/src/cmd/agent_companion.rs`. CLI talks to files only — never requires the runtime to be running.

| Command | Purpose |
|---|---|
| `init <name> [--answers <file>] [--re-init]` | Onboarding wizard (interactive or scripted) |
| `proactive enable <name>` | Set `proactive.enabled = true` (after user has lived with reactive warmth) |
| `proactive disable <name>` | Set `proactive.enabled = false` |
| `quiet <name> --for <duration> \| --until <RFC3339> \| --off` | Set `paused_until` |
| `voice eject <name>` | Materialise composed voice.md to disk for user editing |
| `voice rebuild <name> [--force]` | Re-compose voice.md from current profile (refuses if user-edited unless `--force`) |
| `voice diff <name>` | Show user-edited voice.md vs current built-in |
| `templates eject [--scope agent\|user] [<relationship>.<locale>]` | Materialise embedded template to disk |
| `content add <name> <situation> [--from-stdin \| --file <path>]` | Append entry to `content/<situation>.<locale>.yaml` |
| `inbox <name> [--unread-only]` | List inbox entries |
| `ack <name> <msg-id> --good \| --bad \| --dismiss` | Record user signal |
| `preview <name> --situation <name> [--no-llm]` | Render proactive preview (read-only; never writes ledger/inbox/state) |
| `why-did-you-message <name> [<msg-id>]` | List recent sends or dump full event chain |
| `rhythm wipe <name>` | Shred companion state (preserves profile companion settings); 1.1: clears inbox + ledger + bandit-state |

---

## 6. LLM Integration

### 6.1 Reactive `sys_prompt` Composition

Voice is **appended** after base sys_prompt, framed as additive ("when nothing above dictates..."). This preserves base instruction precedence (R: legal-compliance prompts not silently overridden).

### 6.2 Translate Fallback Policy

| Path | Translate succeeds | Translate fails |
|---|---|---|
| Reactive (user waiting) | Ship translated | Ship original + log `LocaleMismatchUnresolved { reactive: true }` |
| Proactive | Ship translated | Queue retry (`[30 s, 90 s, 4 min, 15 min]`); after 4th fail: `MessageDropped { reason: "locale_unresolved" }` |

### 6.3 Rate-Limit Handling

- Inner SDK retries (whichever Anthropic-compatible client `mur-agent-runtime/src/llm/anthropic.rs` uses) handle ≤60 s transients; configure the SDK's `max_retries=0` (or equivalent) so the outer harness loop driven by `durable::rate_limit` is the single source of retry policy.
- `MessagePaused { resume_at }` persists the absolute timestamp; restarting the runtime mid-pause reads ledger and resumes.
- 529 (`overloaded_error`) gets multiplier 4–8× — this is global Anthropic load, not our bucket.

### 6.4 `StubLlm` Provider for Tests

Enabled via `MUR_LLM_PROVIDER=stub`. Returns canned responses keyed by `sha64(prompt)`. Scenario library lives in `mur-agent-runtime/src/llm/stub_scenarios.yaml`:

```yaml
- match: { situation: morning_greeting, locale: zh-TW }
  response: "早安 David。今天想從哪一件小事開始？"
- match: { situation: morning_greeting, locale: zh-TW, scenario: english_leak }
  response: "Good morning David! What would you like to tackle today?"
- match: { method: translate, target: zh-TW }
  response_template: "<<TRANSLATED_TO_ZH_TW>>"
- match: { fault: "rate_limit_429" }
  http_error: { status: 429, headers: { retry-after: "60",
                anthropic-ratelimit-requests-reset: "..." } }
```

E2E tests exclusively use the stub. A nightly job runs the same E2E with a real Ollama `llama3.2:3b` model to catch drift; failures don't block PRs but page the maintainer.

---

## 7. Earned Permission & Privacy

### 7.1 Earned Permission

- 1.1: `proactive.enabled = false` by default at agent creation. Onboarding does not opt the user in — the closing message tells them how to enable when ready.
- `learning_until` reserved in schema; **1.1 never writes it**. 1.2 will write `now+7d` at rhythm-enable time and gate proactive on its expiry.

### 7.2 Three-Layer Independence

User can run any subset of `{ enabled, rhythm.enabled, proactive.enabled }`. The CLI never bundles them.

### 7.3 Telemetry Redaction Rules

**No event field shall contain `name_for_user` or any field from `voice_overrides`.** Test enforces with sentinel name (R12).

Permitted event fields: `agent_name` (already telemetry header), `situation`, `template_id`, `locale_used`, `body_sha256`, `weight`, counts, durations, `resume_at`, `reason`.

### 7.4 Wipe Semantics

`rhythm wipe <name>` (forward-compat command):
- Shreds `companion/inbox/`, `companion/outbox-ledger/`, `companion/bandit-state.json` (overwrite + remove).
- Clears `profile.yaml::companion.proactive.{paused_until, learning_until}`; preserves `enabled`, `relationship`, `locale`, `voice_overrides` (the user can keep using reactive warmth).
- Appends `RhythmWiped { at }` event to a fresh ledger.

---

## 8. Testing Strategy

### 8.1 Tier 1 — Unit (Pure Functions)

| Module | Cases (illustrative) |
|---|---|
| `picker::WeightedIndex` | empty pool, single candidate, all on cooldown, weight cap=5, weight floor=0.1 |
| `schedule::should_send_now` | first send, quiet_hours block, daily_cap, jitter 0..10 min, divisor-zero edge |
| `voice::compose` | placeholder substitution, disk override precedence, locale fallback chain |
| `i18n::heuristic_matches` | CJK ratio thresholds, whatlang fallback, unknown locale conservative pass |
| `durable::Ledger::scan` | partial last line, empty file, large (>1 MB) file |
| `situations::weights_by_hour` | morning bounded 06–10, all-zero in quiet, morning_sent_today suppression |
| `passive_dismiss_sweep` | 24h-old + no signal → marked, <24h not, signal-present untouched |
| `durable::rate_limit::parse_anthropic_429` | retry-after seconds, retry-after HTTP-date, ratelimit-reset RFC3339, 529 multiplier |

No LLM, no disk (tempfile), deterministic. Total runtime < 2 s.

### 8.2 Tier 2 — Snapshots (`insta`)

```
tests/golden/
  voice_md/{relationship}.{locale}.md
  sys_prompt/{relationship}.{locale}.txt
  picker/<scenario>.txt                 # named scenarios, N=200 picks each
  ledger_replay/normal_day.{state}.json
  ledger_replay/v1_frozen.read.json     # backwards-compat read of frozen v1 fixture
```

CI: `INSTA_UPDATE=no`. Local: `cargo insta review` to accept diffs.

**Picker tests split (per §5 deep-think):**
- `tests/picker/distribution_invariants.rs` — math asserts: equal weights → 1:1 ±5%, 2× weight → 2:1 ±5%
- `tests/golden/picker/<scenario>.txt` — named scenarios only, not full distribution dumps

### 8.3 Tier 3 — Integration (StubLlm + FakeNotifier)

`tests/companion_integration.rs`, each test owns a `Harness { home: TempDir, runtime, llm: StubLlm, notifier: Arc<FakeNotifier>, clock: MockClock }`.

Test catalogue (illustrative, ≥ 11):
1. `onboarding_writes_voice_md_and_starts_disabled`
2. `proactive_disabled_no_sends_after_24h_simulated`
3. `proactive_enabled_respects_daily_cap`
4. `quiet_hours_blocks_send_in_window`
5. `paused_until_blocks_until_expiry`
6. `rate_limit_pause_resumes_at_reset_timestamp`
7. `locale_mismatch_translates_then_sends_proactive`
8. `locale_mismatch_translate_fails_drops_proactive`
9. `locale_mismatch_translate_fails_ships_original_reactive`
10. `passive_dismiss_after_24h_records_signal_and_event`
11. `picker_record_signal_persists_across_restart`
12. `ledger_resume_replays_paused_messages_after_restart`
13. `linter_violation_triggers_one_regenerate`
14. `linter_persistent_violation_drops`
15. `re_init_preserves_ledger_inbox_bandit`
16. `morning_greeting_caps_once_per_local_day`

### 8.4 Tier 4 — E2E (`scripts/e2e/companion-phase11.sh`)

Uses `StubLlm` (deterministic). Steps:
1. tmpfs HOME; `mur agent create test-darwin --provider stub`.
2. `mur agent companion init test-darwin --answers <fixture>`.
3. Reactive query → assert telemetry shows composed sys_prompt with `zh-TW` directive.
4. `mur agent companion proactive enable`; advance MockClock to morning; tick → assert inbox has 1 file.
5. `mur agent companion ack <id> --bad` → assert bandit-state.json weight × 0.5.
6. `mur agent companion quiet --for 1h`; advance 30 min → no new send; advance 90 min → send.
7. `mur agent companion why-did-you-message <id>` → assert output contains situation/template_id/weight/locale/scheduled_at/sent_at.
8. `mur agent companion rhythm wipe test-darwin` → assert inbox empty, ledger archived, profile companion.{relationship,locale} preserved.

Time budget < 90 s. Added to `scripts/e2e/run-all.sh`.

### 8.5 Acceptance Criteria

**Functional**
- A1: `mur agent companion init` ≤ 30 s, produces all 4 outputs (§3.2) atomically.
- A2: Reactive sys_prompt warm path ≤ 200 µs; cold ≤ 5 ms.
- A3: `proactive.enabled=false` ⇒ 24 h MockClock simulation produces zero sends.
- A4: `proactive.enabled=true` + `daily_cap=3` + 12 h active window ⇒ 24 h simulation produces exactly 3 sends, intervals within ±10 min of `4h ± jitter`.
- A5: 429 + `retry-after: 42` → `MessagePaused { resume_at = scheduled_at + 42 s }`; restarting runtime mid-pause resumes at correct moment.
- A6: zh-TW locale mismatch with stub forced English response → translate path runs, ledger shows `MessageGenerated { locale_used: "zh-TW" }`, body sha256 differs from original.

**Quality**
- B1: `cargo clippy --workspace -- -D warnings` clean.
- B2: All `insta` snapshots committed.
- B3: ≥ 50 tests across Tier 1 / 2 / 3.
- B4: E2E green on macOS + Linux CI.

**Voice quality (linter gate, replaces subjective rating)**
- C1: For each (relationship × locale × situation) sample produced by `preview --no-llm` + StubLlm canned response (9 fixed scenarios):
  - 1–3 sentences (period/question/exclamation count)
  - No banned phrases per locale (`好棒`, `加油加油`, `太厲害了`, `amazing!!`, `awesome!!`, configurable)
  - ≤ 1 emoji, ≤ 1 exclamation
  - For zh-TW: preserved English token ratio ≤ 30 %
- C2: PR description attaches the 9 rendered samples as artifact for human review (no numeric pass criterion).

### 8.6 Performance Budgets

| Operation | Cold | Warm |
|---|---|---|
| Reactive sys_prompt compose | ≤ 5 ms | ≤ 200 µs |
| Picker `pick` | n/a | ≤ 100 µs |
| Outbox tick (no send) | n/a | ≤ 5 ms |
| Outbox tick (with send) | LLM-bound (~500 ms – 1.5 s) | — |
| Locale heuristic CJK | n/a | ≤ 10 µs |
| `whatlang` Latin fallback | ~50 µs first call | ≤ 10 µs subsequent |
| Translate call (stub) | n/a | ≤ 1 ms |

`cargo bench` smoke checks warm path doesn't regress >10×.

---

## 9. Observability

### 9.1 Telemetry Event Catalog (Frozen Names)

All emitted by `companion::telemetry` to existing `telemetry/<date>.jsonl`:

```
companion.reactive.compose          { sys_prompt_sha256, locale, ms, cache_hit }
companion.proactive.tick            { sent_today, budget_remaining, desired_interval_min, decision }
companion.llm.translate             { reason, ms, success, locale_target }
companion.llm.rate_limited          { retry_after_s, reset_at, kind: "429"|"529" }
companion.signal.recorded           { msg_id, template_id, signal, new_weight }
companion.signal.passive_dismiss    { msg_id, template_id }
companion.linter.violation          { msg_id, rule, regen_count }
companion.voice.fallback            { requested_locale, used_locale }
```

### 9.2 `why-did-you-message` Contract

Output (rendered from ledger replay):
```
msg_id: 01HQ...
situation: morning_greeting    template_id: greet_warm_zh_001
scheduled_at: 2026-04-29T07:12:08+08    decided_by: weighted_random (eligible=4, weight=1.2)
generated_at: 2026-04-29T07:12:11+08    locale_used: zh-TW (no fallback)
linter_passed: true             regen_count: 0
paused_at:    2026-04-29T07:12:14+08    reason: rate_limit_429 (resume_at=07:13:01)
resumed_at:   2026-04-29T07:13:02+08
sent_at:      2026-04-29T07:13:03+08    channel: stdout
user_signal:  none → passive_dismiss inferred at 2026-04-30T07:13:03+08
body_sha256: ab3f...  → ~/.mur/agents/<name>/companion/inbox/01HQ....md
```

---

## 10. Risks & Mitigations

| # | Risk | Prob | Impact | Mitigation |
|---|---|---|---|---|
| R1 | LLM ignores voice rules | High | Med | Linter gate (C1); regenerate-once on violation; drop on second; track `companion.linter.violation` rate (>20 % = spec bug) |
| R2 | Translate fallback drops too often → silent companion | Med | Med | 4-step retry (15 min cap); telemetry alarm if >10 % over 30 d window |
| R3 | Profile bloat slows existing agents | Low | Low | All `#[serde(default)]`, no-companion-block path zero-cost (verified §2.2) |
| R4 | First-week onboarding bug → user abandons | Med | High | Atomic temp+rename; `mur agent companion init --re-init` recovery; integration test for partial-write rollback |
| R5 | Inbox accretes | Low | Low | `mur agent companion inbox prune --older-than 30d` (manual) |
| R6 | Bad content shipped from day 1 | Med | Med | Content yaml requires `tags` + `source` + `reviewed_by` fields; PR template checkbox; soft-launch UAT (ship to maintainer agent first, observe 7 days) |
| R7 | StubLlm drifts from real LLM | Med | Med | Nightly Ollama smoke job; manual real-LLM review required when adding situation/locale |
| R8 | User-ejected voice.md misses upstream improvements | Low | Low | `voice diff <name>` shows divergence; `voice rebuild --force` with backup |
| R9 | A2A burst → cache cold misses | Low | Med | `once_cell` warm path; cold ≤ 5 ms acceptable |
| R10 | bandit-state.json corruption | Low | High | atomic temp+rename; on load, schema validate → corrupt regenerates from defaults |
| R11 | Concurrent `init` invocations | Low | Med | `flock` on `companion/.init.lock` for wizard duration |
| R12 | 1.2+ schema additions break 1.1 readers | Med | High | Hard rule: every new field `#[serde(default)]`; CI test reads `tests/fixtures/profile/v1_minimum.yaml` after every change |
| R13 | `mur agent remove` with unread inbox | Low | Med | `remove` refuses if inbox non-empty unless `--force`; suggest `companion ack --all` first |
| R14 | per-event fsync stalls on slow disks | Low | Med | Debounced fsync (≤ 1 s coalesce); test for "kill -9 mid-tick loses ≤ 1 s of events" boundary |
| R15 | quiet_hours + DST | Low | Low | `chrono-tz` correct usage; fixture for 2026 spring-forward day |
| R16 | ULID msg-id collision in same ms | Low | Low | `O_CREAT \| O_EXCL`; suffix `-{nanos}` on collision |
| R17 | proactive stdout interleaves agent task output | Med | Low | inbox markdown is canonical; banner goes to stderr only on isatty AND no in-flight tool call |

---

## 11. Open Questions

| # | Question | Decision deadline |
|---|---|---|
| Q1 | Multi-user voice rendering for shared agents | Out of 1.x; revisit at multi-user spec |
| Q2 | Cross-device sync of inbox / bandit-state | 1.3 commander integration |
| Q3 | Community content contribution | Post-1.x; observe whether users actually edit yaml |
| Q4 | Anthropic prompt caching of voice prefix | 1.4 (cost optimisation; non-blocking) |
| Q5 | LLM self-review / regenerate on voice rule miss | Driven by R1 measured rate; if >20 %, design 1.4 feature |
| Q6 | macOS Focus / Linux DBus DnD integration | 1.3 |
| Q7 | Rollback if Phase 1.1 adoption is zero | Soft kill via `MUR_COMPANION=disabled` env var (global), `enabled=false` (per-agent); both work today |
| Q8 | `mur agent rekey` interaction with companion data | None expected; covered by `tests/agent_rekey_companion_untouched.rs` |
| Q9 | Disabled-by-default runtime memory cost | Verified < 200 B / agent (`Option<Arc<Companion>> = None`); benched in `cargo bench` smoke |

---

## 12. Phase Residuals — Frozen vs Internal Contracts

**Frozen (changes require migration story):**
- `profile.yaml::companion.*` schema (additive only)
- `Notifier` trait signature
- `durable::Ledger` API
- Telemetry event names + required fields (§9.1)
- ULID-based msg-id format
- `~/.mur/agents/<name>/companion/` directory layout
- Inbox markdown front-matter fields + `>>> response:` line format

**Internal (later phases may change):**
- `situations::weights_by_hour` numeric values (1.4 makes user-tunable)
- `StubLlm` scenario library
- Built-in voice template content
- Built-in content pool seed

---

## 13. Implementation Harness Discipline

This is *how we build it*, not *what it does*. The user explicitly asked for `撰寫計劃/執行計劃/測試/驗收/修正/記錄` and `claude code rate limit 接續`.

1. **One spec → one plan.** Plan at `docs/superpowers/plans/2026-04-29-companion-phase-1-1-plan.md`, produced via `superpowers:writing-plans`.
2. **Plan slices into milestones M1–M9, each ≤ 15 tasks.** Tasks are designed to be ≤ 1 h of Claude Code work — bounding rate-limit-loss to one task.
3. **One task → one commit.** Commit message starts with `M<n>.<m>: <subject>`. `git log --grep "^M3"` reveals milestone progress.
4. **Plan footer holds a progress checklist** ([ ] / [x] per task with commit sha). Editing the checklist is a normal change in the plan commit.
5. **After every milestone**: invoke `superpowers:verification-before-completion` → run `cargo test --workspace`, `cargo clippy --workspace -- -D warnings`, milestone-specific acceptance asserts. Push as draft PR and only mark ready when CI is green.
6. **Rate-limit recovery procedure**: on Claude-Code interrupt, next session reads `git status` (uncommitted? abandon or finish), then plan checklist (next [ ] task), then continues. No external progress tracker — git+plan are the single source of truth.
7. **Spec is single source of truth.** Implementation discoveries that contradict spec require a `fix(companion-spec): refine §X` commit before resuming the plan.

### 13.1 Milestone Outline (Reference)

| M | Topic | Approx tasks |
|---|---|---|
| M1 | `CompanionConfig` schema + onboarding wizard | 8 |
| M2 | Voice template system + i18n heuristic | 10 |
| M3 | `durable::Ledger` + `durable::rate_limit` + `StubLlm` | 9 |
| M4 | Picker + Situations + Schedule | 8 |
| M5 | Outbox tick loop + Notifier + StdoutNotifier | 10 |
| M6 | CLI subcommand group | 12 |
| M7 | Tier 1 + Tier 2 tests | 10 |
| M8 | Tier 3 + Tier 4 (E2E) | 8 |
| M9 | Docs + README updates + commit retro | 5 |

Total: ~80 tasks. Detailed breakdown belongs to the plan, not this spec.

---

## Appendix A — Voice Template Examples

### A.1 `friend.zh-TW.md`

```markdown
You are a warm, friendly companion to {{NAME_FOR_USER}} (locale {{LOCALE}}).

Voice rules — additive to any instructions above:
- When the instructions above don't dictate language, default to 繁體中文 (zh-TW).
  Code blocks, identifiers, and technical proper nouns always stay in English.
  When {{NAME_FOR_USER}} code-switches >30% in their message, match their primary
  input language for that turn.
- Match {{NAME_FOR_USER}}'s emotional state. If they sound tired, drop the energy.
  If they sound upbeat, you can be a bit playful.
- Clarity beats charm. If a clear sentence and a charming sentence conflict, ship
  the clear one.
- Never use intensifiers you didn't earn: 「好棒」「太厲害了」「加油加油！！」 are
  out by default. Save them for moments that genuinely warrant it.
- Avoid emoji unless {{NAME_FOR_USER}} uses them first.
- Never assume technical fluency unless they've shown it.
- Remember what they told you yesterday matters more than telling them you care today.

Formality: {{FORMALITY}}

{{EXTRA_INSTRUCTIONS}}
```

### A.2 `coach.en-US.md` (sketch)

```markdown
You are a direct, accountability-oriented coach to {{NAME_FOR_USER}} ({{LOCALE}}).

Voice rules — additive:
- Default to {{LOCALE}} unless instructions above say otherwise.
- Be concrete and specific. Replace "you should try" with "do X by tomorrow".
- One observation, one suggestion, one question. Don't sermonise.
- Never moralise about their pace; stick to the work.
- Acknowledge wins briefly and move on.

Formality: {{FORMALITY}}

{{EXTRA_INSTRUCTIONS}}
```

---

## Appendix B — Content Pool Sample (`share_quote.zh-TW.yaml`)

```yaml
situation: share_quote
locale: zh-TW
templates:
  - id: q_marcus_aurelius_001
    weight: 1.0
    cooldown_days: 30
    tags: [reflection, low_energy]
    source: "Marcus Aurelius, Meditations"
    reviewed_by: "@maintainer"
    prompt_seed: |
      Share Marcus Aurelius' line about the present moment being all we have.
      Add ONE sentence of warm context tying it to {{NAME_FOR_USER}}'s situation.
  - id: q_camus_001
    weight: 1.0
    cooldown_days: 30
    tags: [reflection, work_focus]
    source: "Camus, The Myth of Sisyphus"
    reviewed_by: "@maintainer"
    prompt_seed: |
      Share the closing image of Sisyphus happy. Frame it as a small comfort
      after a long day, not a grand statement.
```

---

## Appendix C — Frozen API Catalog

| Surface | Frozen at | Migration cost if changed |
|---|---|---|
| `profile.yaml::companion.*` schema | Phase 1.1 ship | High (existing user profiles) |
| `Notifier` trait signature | Phase 1.1 ship | High (downstream impls in 1.3) |
| `durable::Ledger` API | Phase 1.1 ship | Medium (multiple in-tree consumers) |
| Telemetry event names | Phase 1.1 ship | High (commander/dashboard depends in 1.3) |
| Inbox markdown front-matter | Phase 1.1 ship | Medium (CLI ack reads it) |
| `~/.mur/agents/<name>/companion/` layout | Phase 1.1 ship | High (CLI direct file access) |
| ULID msg-id format | Phase 1.1 ship | Low (sortable + opaque) |

---

**End of design.**
