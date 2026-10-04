//! Task state machine and orchestration (§8.3).
//! P0a only implements `run_sync` fully; streaming is P0b.

use crate::hitl::HitlApprovals;
use crate::hooks::{HookChain, HookCtx, PromptView, ToolCall, ToolResult};
use crate::llm::{LlmClient, LlmError, LlmRequest, RequestIntent};
use crate::skills::RuntimeSkills;
use crate::skills::injector::inject_layer2;
use crate::skills::trigger_matcher::{format_layer3, layer3_body, match_prompt};
use crate::telemetry_writer::{Event, SkillOutcome};
use mur_common::a2a::{Message, MessagePart, Task, TaskError, TaskState};
use mur_common::config::SkillsConfig;
use mur_common::skill::McpInventory;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

mod agentic_loop;
mod builder;
mod conversation;
mod helpers;
mod run;
mod step;
mod system_prompt;

use conversation::*;
pub(crate) use helpers::*;
use system_prompt::*;

#[derive(Debug, Clone)]
pub struct TaskSpec {
    pub input: Message,
    pub context_task_id: Option<String>,
    /// Caller-supplied task id. When `Some`, the runner uses it verbatim so the
    /// client can cancel by an id it already holds; when `None` the runner
    /// generates one (back-compatible).
    pub task_id: Option<String>,
    /// Active fleet name for this turn, derived from a `fleet-<name>` channel id
    /// by the `channel/delegate` handler. Drives fleet-scoped skill injection;
    /// `None` for non-fleet turns, so fleet-scoped skills stay hidden outside
    /// their fleet (fail-closed).
    pub active_fleet: Option<String>,
    /// Active team id for this turn, derived from the fleet's `team_id` field
    /// by the `channel/delegate` handler. Drives team-scoped skill injection;
    /// `None` for non-fleet turns or fleets without a team (fail-closed).
    pub active_team: Option<String>,
    /// Why this turn is being run (see `RequestIntent`). Deliberately NOT
    /// `Default`-derived on `TaskSpec` — every construction site must state
    /// its intent explicitly so an interactive (user-facing) call site can
    /// never silently fall through to `Background` (which would make it
    /// eligible for Smart cheap-model routing). Runtime-initiated call
    /// sites (cron scheduler, idle scheduler, watch scheduler) tag
    /// `Background`; chat / A2A `message/send` / `channel/delegate` tag
    /// `Interactive`.
    pub intent: RequestIntent,
    /// When set, the LLM is instructed to write its complete output to this
    /// file path and return only the path in its reply. After the task
    /// completes, the runtime verifies the file exists, computes its hash,
    /// populates `Task.artifacts`, and replaces the assistant reply with a
    /// short `[File: path]` reference. Callers then read the file
    /// byte-by-byte instead of re-typing content through another LLM
    /// (issue #715 Part B).
    pub output_artifact_path: Option<std::path::PathBuf>,
    /// The caller's working directory for this turn (`context.cwd` on the
    /// wire). The harness owns the working directory, not the model: when set
    /// and entitled, it becomes the session cwd the tools resolve against and
    /// is declared in the system prompt every turn. `None` leaves the session
    /// cwd where it is.
    pub cwd: Option<std::path::PathBuf>,
    /// Is a person watching this turn and able to stop it by hand? Attended
    /// turns have no deadline and a stuck clock that only warns (spec §3.2).
    /// Deliberately not defaulted: every construction site says which it is,
    /// the way it already says `intent`. `message/send` passes its
    /// `can_approve`; `channel/delegate` and every runtime scheduler pass
    /// `false`.
    pub attended: bool,
    /// The launching scope's REMAINING clock, in seconds — a fleet delegating
    /// with twelve minutes left passes 720 (spec §3.4). `None` = resolve the
    /// deadline from `profile.yaml` → `config.yaml` → built-in.
    pub deadline_secs: Option<u64>,
}

#[derive(Debug)]
pub enum TaskOutcome {
    Completed(Task),
    Failed(Task),
    Cancelled(Task),
}

#[derive(Clone)]
pub enum RunnerBackend {
    StubEcho,
    StubSlow,
    /// The agent's configured model provider has no client in this runtime
    /// (e.g. `deepseek`). Rather than silently echoing input — which looks
    /// alive but parrots — every turn replies with this misconfiguration
    /// message so the user sees exactly what to fix.
    Misconfigured(String),
    Llm(Arc<dyn LlmClient>),
    /// The turn runs inside a spawned coding CLI, which owns the loop while
    /// MUR owns the tools. Not an `LlmClient`: there is no completion call to
    /// make here, so it cannot be one of those.
    CliSpawn(&'static mur_common::cli_backend::CliBackend),
}

/// Cap on task-state entries retained in memory. Oldest entries are evicted
/// when this limit is exceeded so long-lived agents don't leak unboundedly.
const MAX_REGISTRY_ENTRIES: usize = 1_024;

/// Cap on chat threads retained for multi-turn memory (LRU-evicted) so a
/// long-lived agent serving many conversations doesn't leak unboundedly.
const MAX_CONVERSATIONS: usize = 256;
/// Hard ceiling on stored messages per conversation, kept only so a pathological
/// stream of empty turns cannot grow the vector without bound. The real limit is
/// [`CONV_BUDGET_DIVISOR`] below — a turn count says nothing about how much
/// context the turns occupy (issue #1200). Applied in whole turns (see
/// `remember`), so the history always starts on a `user` message (Anthropic
/// requires that).
const MAX_CONV_MESSAGES: usize = 400;

/// Characters per token, for sizing stored history against a token budget.
/// Deliberately crude: the alternative is tokenizing every stored turn on every
/// send, and the budget leaves enough headroom that a 25% error costs nothing.
const CHARS_PER_TOKEN_ESTIMATE: usize = 4;

/// Share of the model's context window that stored history may occupy: the
/// window also has to hold the system prompt, injected skills, the tool
/// inventory, this turn's tool traffic and the reply, so history gets a quarter.
const CONV_BUDGET_DIVISOR: u64 = 4;

/// History budget when the model's context window is unknown — an unregistered
/// model, or a registry entry the catalog never carried a window for. Sized for
/// the smallest window still in common use (32k) under the same quarter share.
const DEFAULT_CONV_BUDGET_TOKENS: u64 = 8_000;

/// Where a turn's approval prompt goes, and whether anyone there can answer it.
///
/// The bool is the half that matters: a one-shot `mur agent send` receives
/// deltas and step events perfectly well, so the sink alone cannot tell it from
/// a murmur TUI with a human watching.
pub(crate) type ApprovalSink = (tokio::sync::mpsc::Sender<serde_json::Value>, bool);

pub struct TaskRunner {
    backend: RunnerBackend,
    registry: Arc<Mutex<HashMap<String, TaskState>>>,
    /// Insertion-order index used to evict the oldest entry when `registry`
    /// exceeds `MAX_REGISTRY_ENTRIES`.
    registry_keys: Arc<Mutex<VecDeque<String>>>,
    cancel_signals: Arc<Mutex<HashMap<String, oneshot::Sender<()>>>>,
    telemetry: Option<mpsc::Sender<Event>>,
    system_prompt: Option<String>,
    last_activity_at: Arc<AtomicI64>,
    skills: Option<Arc<RuntimeSkills>>,
    skills_cfg: SkillsConfig,
    memory_cfg: mur_common::config::MemoryConfig,
    recently_fired: Mutex<VecDeque<(u64, String)>>,
    turn_counter: AtomicU64,
    cumulative_input_tokens: AtomicU64,
    /// Parallel to `cumulative_input_tokens` (runner-lifetime, shared across
    /// tasks). Not used by the loop's own input-only token budget; it exists so
    /// `run_sync_inner` can snapshot-delta both counters and report a turn's
    /// real input+output token usage in `Task.usage` (fleet cost accounting).
    cumulative_output_tokens: AtomicU64,
    /// Input-token count of the most recent single LLM call. Approximates the
    /// current context window fill, unlike the cumulative total. Read by the
    /// `token_usage` closure to populate `context_tokens` in per-turn usage JSON
    /// so the CLI glass-box bar can show a live context gauge.
    last_input_tokens: AtomicU64,
    /// `model_ref` of the most recent successful LLM response, read by the
    /// `token_usage` closure to populate `Task.usage.model_ref`. Reset to
    /// `None` at the start of each `run_sync_inner` turn so a stub/misconfigured
    /// backend (which never calls a real model) reports no model rather than a
    /// stale one from a previous turn. Same runner-lifetime concurrency caveat
    /// as `cumulative_input_tokens`: overlapping turns on one runner can race
    /// this value; acceptable for a best-effort telemetry field.
    last_model_ref: Mutex<Option<String>>,
    /// True when the most recent LLM response of the current turn was cut off
    /// at the provider's max_tokens ceiling (`StopReason::MaxTokens`). Read by
    /// the `token_usage` closure to add `"truncated": true` to `Task.usage`;
    /// reset alongside `last_model_ref` at the start of each `run_sync_inner`
    /// turn. Same runner-lifetime concurrency caveat as `last_model_ref`.
    last_turn_truncated: AtomicBool,
    hook_chain: Option<Arc<HookChain>>,
    hook_ctx: Option<HookCtx>,
    hook_cancel: Option<CancellationToken>,
    pending_approvals: Option<HitlApprovals>,
    /// One-time shim tickets for CLI-spawn turns (`hitl::shim_ticket`).
    shim_trust: crate::hitl::shim_ticket::ShimTrust,
    /// P3: settled chat-gate decisions (gate B memory). `None` = ask every time.
    decision_store: Option<Arc<dyn crate::hitl::store::DecisionStore>>,
    /// The agent's own name, for `chat_action_hash`. Empty until the supervisor
    /// sets it; an empty name still hashes, it just never matches another agent.
    agent_name: String,
    notifier: Option<tokio::sync::mpsc::Sender<serde_json::Value>>,
    /// Per-turn client notifiers keyed by task id, registered by `message/send`
    /// so a tool-approval prompt is routed to the connection that issued the
    /// turn instead of broadcast to every client. Falls back to `notifier`.
    ///
    /// The flag is whether that connection can actually answer an approval
    /// prompt. A one-shot `mur agent send` supplies a sink like any other
    /// client — deltas and step events reach it fine — but nobody is reading
    /// for a `y`. Without this the gate could not tell the two apart and waited
    /// the full `hitl.timeout_secs` for an answer that was never coming.
    client_notifiers: Arc<tokio::sync::Mutex<HashMap<String, ApprovalSink>>>,
    /// Who may answer an approval for a task, as distinct from who is
    /// watching it. Same connection for an in-process turn; different ones
    /// for a CLI-spawn turn, where the shim can answer but the audience is
    /// whoever ran `mur agent send`.
    approval_sinks: Arc<tokio::sync::Mutex<HashMap<String, ApprovalSink>>>,
    /// Per-turn steering channels keyed by task id. A running agentic loop
    /// holds the receiver; `turn/steer` pushes a user interjection here and the
    /// loop picks it up at the next iteration boundary.
    steering: Arc<tokio::sync::Mutex<HashMap<String, tokio::sync::mpsc::Sender<String>>>>,
    hitl_timeout_secs: u32,
    /// B1 sandbox enforcing? `false` refuses every `Ask` tool (D2 / D2b).
    sandbox_enforcing: bool,
    /// The two scopes this process can see — `config.yaml limits:` and the
    /// agent's own `profile.yaml limits:` — resolved per turn in `bounds_for`.
    limits: (
        mur_common::limits::Limits,
        Option<mur_common::limits::Limits>,
    ),
    iteration_ceiling: u32,
    /// How far this agent carries a turn before handing back (issue #001).
    /// Strict by default; only a profile that says `autonomy: continue` gets
    /// the nudge.
    autonomy: mur_common::hitl::Autonomy,
    tools: Vec<Arc<dyn crate::tools::ToolExecutor>>,
    tools_policy: Vec<mur_common::agent::ToolRule>,
    socket_path: Option<std::path::PathBuf>,
    /// Credentials the user handed the agent. Names go into the system prompt;
    /// values are masked out of every tool result. `None` = no vault (stubs).
    secrets: Option<Arc<crate::secrets::SecretVault>>,
    /// Per-agent scratch dir named in the output-locations rule. `None` =
    /// not granted (or a stub), so the prompt omits the scratch line.
    scratch_dir: Option<std::path::PathBuf>,
    /// The bash job table (spec D3/D8), so the loop can end a task's jobs on
    /// an unattended stop or a cancel, and the supervisor every job at exit.
    bash_jobs: Option<Arc<crate::tools::bash_jobs::JobTable>>,
    /// Per-agent effort for this agent's own turns. `None` = the API default.
    /// Mechanical internal calls override it downward regardless (see
    /// `graceful_exit`).
    ///
    /// Interior-mutable because murmur's `/effort` changes it on a RUNNING
    /// agent through the `effort/set` A2A method, and the runner is shared as
    /// `Arc<TaskRunner>`. Unlike a model swap this needs no reconstruction:
    /// effort is a per-call parameter, so the next request picks it up.
    ///
    /// Deliberately stores what was ASKED FOR, not a narrowed value. Narrowing
    /// to what a model accepts happens at the wire, in each client — see
    /// `mur_common::llm::supported_effort`, whose doc records that split. A
    /// second narrowing here would be a second derivation of one rule.
    effort: std::sync::RwLock<Option<mur_common::llm::Effort>>,
    /// Multi-turn chat memory keyed by `context.task_id` (see `ConversationStore`).
    conversations: Mutex<ConversationStore>,
    /// The session cwd shared with bash and the file tools, plus the roots a
    /// caller-supplied `TaskSpec.cwd` may move it to. `None` for runners built
    /// without tools (stubs, most tests): no cwd line in the prompt.
    session_cwd: Option<(crate::tools::fs_policy::SessionCwd, Vec<String>)>,
    /// Reads the working project's `AGENTS.md` / `CLAUDE.md` into the prompt,
    /// through the same entitlement gate as `read_file`. `None` (stubs, tests)
    /// or no `session_cwd` means no block.
    project_instructions: Option<crate::project_instructions::ProjectInstructions>,
    /// Set by `begin_drain()` during graceful shutdown. When true, `run_sync_inner`
    /// rejects new turns immediately with a transient failure so in-flight work
    /// can finish before transports are torn down.
    draining: Arc<AtomicBool>,
}

/// Diagnostic ceiling on agentic-loop iterations (spec §6). Not a setting:
/// it exists so a runaway bug becomes a stop with a reason instead of a hang.
/// Bounds that a user configures are `deadline` and `stuck` (`mur limits`).
pub const ITERATION_CEILING: u32 = 10_000;

/// Rolling-window size for doom-loop detection: the last N tool-call
/// fingerprints are retained.
const LOOP_WINDOW: usize = 8;

/// Number of identical tool-call fingerprints within the rolling window that
/// trips the doom-loop guard.
const LOOP_REPEAT_THRESHOLD: usize = 3;

/// Max number of times a single turn retries an `LlmError::RateLimit` (HTTP
/// 429) before giving up and propagating the error. Separate from the
/// empty-stream retry's own attempt counter.
const MAX_RATE_LIMIT_RETRIES: u8 = 3;

/// Base delay for the rate-limit backoff: attempt N sleeps
/// `RATE_LIMIT_BACKOFF_BASE * 2^N` (1-indexed attempts give 2s, 4s, 8s, ...).
const RATE_LIMIT_BACKOFF_BASE: std::time::Duration = std::time::Duration::from_secs(1);

/// Why the agentic loop stopped short of a natural end-turn. Carried out of the
/// loop into the task's `usage` JSON so callers can tell a clean completion from
/// a budget-truncated one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopStop {
    /// `ITERATION_CEILING` — a runaway, not a budget.
    IterationCeiling,
    LoopDetected,
    Deadline,
    Stuck,
    /// The model kept calling a tool that was withdrawn this turn. Every such
    /// call is refused without running anything, so further iterations cannot
    /// make progress — and the doom-loop detector will not catch it quickly,
    /// because it fingerprints (tool, ARGS, result) and a model that varies
    /// its arguments produces a fresh fingerprint each time. That is how a
    /// withdrawn `bash` still burned 54 iterations on 2026-09-13.
    ToolWithdrawn,
}

impl LoopStop {
    fn as_str(self) -> &'static str {
        match self {
            LoopStop::IterationCeiling => "iteration_ceiling",
            LoopStop::LoopDetected => "loop_detected",
            LoopStop::Deadline => "deadline",
            LoopStop::Stuck => "stuck",
            LoopStop::ToolWithdrawn => "tool_withdrawn",
        }
    }
}

/// An early, graceful termination of the agentic loop: which budget tripped and
/// how many iterations had completed. `None` (no `LoopExit`) means the model
/// ended the turn naturally.
#[derive(Debug, Clone, Copy)]
struct LoopExit {
    reason: LoopStop,
    iterations: u32,
}

impl TaskRunner {}

pub struct AsyncTaskHandle {
    id: String,
    done: oneshot::Receiver<TaskOutcome>,
}

impl AsyncTaskHandle {
    pub fn task_id(&self) -> &str {
        &self.id
    }

    pub async fn await_completion(self) -> TaskOutcome {
        self.done.await.unwrap_or_else(|_| {
            TaskOutcome::Failed(Task {
                id: self.id,
                artifacts: None,
                state: TaskState::Failed,
                messages: vec![],
                created_at: chrono::Utc::now().to_rfc3339(),
                completed_at: None,
                error: None,
                usage: None,
            })
        })
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod step_tests {
    use super::{STEP_MAX_BYTES, cap_step_output, step_notification};

    #[test]
    fn notification_has_jsonrpc_envelope_and_method() {
        let n = step_notification("step/started", serde_json::json!({ "step_id": "s1" }));
        assert_eq!(n["jsonrpc"], "2.0");
        assert_eq!(n["method"], "step/started");
        assert_eq!(n["params"]["step_id"], "s1");
    }

    #[test]
    fn cap_step_output_short_unchanged() {
        let (out, truncated, full_len) = cap_step_output("hello");
        assert_eq!(out, "hello");
        assert!(!truncated);
        assert_eq!(full_len, 5);
    }

    #[test]
    fn cap_step_output_long_is_truncated() {
        let big = "é".repeat(STEP_MAX_BYTES); // 2 bytes/char → over the cap
        let (out, truncated, full_len) = cap_step_output(&big);
        assert!(truncated);
        assert_eq!(full_len, big.len());
        assert!(out.is_char_boundary(out.len())); // never split a char
    }
}

/// #940: the deny path stuttered — `tool call denied: denied`.
#[cfg(test)]
mod deny_message_tests {
    use super::deny_message;

    #[test]
    fn a_bare_denial_is_not_repeated() {
        // What the CLI sends for a plain `n` press.
        assert_eq!(deny_message(Some("denied")), "tool call denied");
        assert_eq!(deny_message(Some("  denied  ")), "tool call denied");
        assert_eq!(deny_message(Some("")), "tool call denied");
        assert_eq!(deny_message(None), "tool call denied");
    }

    #[test]
    fn a_reason_that_says_something_survives() {
        assert_eq!(
            deny_message(Some("timed out")),
            "tool call denied: timed out"
        );
        assert_eq!(
            deny_message(Some("no approval channel available")),
            "tool call denied: no approval channel available"
        );
    }
}

#[cfg(test)]
mod tool_policy_tests {
    use super::effective_tool_policy;
    use mur_common::agent::{ToolPolicy, ToolRule};

    fn rule(pattern: &str, policy: ToolPolicy) -> ToolRule {
        ToolRule {
            pattern: pattern.into(),
            policy,
            risk: Default::default(),
        }
    }

    /// The fix: with no rule, `recall` runs instead of parking for
    /// `hitl.timeout_secs` on a path where nothing can approve.
    #[test]
    fn recall_defaults_to_allow() {
        assert_eq!(effective_tool_policy(&[], "recall"), ToolPolicy::Allow);
    }

    /// The guard that matters. An exemption sets a DEFAULT; an operator's
    /// explicit rule must still decide. Written at the call site because the
    /// earlier inline form compiled fine with this half deleted.
    #[test]
    fn an_explicit_rule_beats_the_exemption() {
        assert_eq!(
            effective_tool_policy(&[rule("recall", ToolPolicy::Deny)], "recall"),
            ToolPolicy::Deny
        );
        assert_eq!(
            effective_tool_policy(&[rule("rec*", ToolPolicy::Deny)], "recall"),
            ToolPolicy::Deny,
            "a wildcard rule counts as explicit too"
        );
    }

    /// Control: everything else stays fail-closed. If this flips, the
    /// exemption has leaked into a general "allow unknown tools".
    #[test]
    fn unknown_tools_still_ask() {
        for name in [
            "bash",
            "remember",
            "fleet_run",
            "parallel_jobs",
            "recall_all",
        ] {
            assert_eq!(
                effective_tool_policy(&[], name),
                ToolPolicy::Ask,
                "{name} must stay gated"
            );
        }
    }

    #[test]
    fn suggest_replies_is_still_exempt() {
        assert_eq!(
            effective_tool_policy(&[], "suggest_replies"),
            ToolPolicy::Allow
        );
    }

    fn risky(pattern: &str, policy: ToolPolicy, risk: RiskTier) -> ToolRule {
        ToolRule {
            pattern: pattern.into(),
            policy,
            risk: Some(risk),
        }
    }

    use mur_common::hitl::RiskTier;

    /// #1600: `allow` + a declared tier above Write asks first.
    #[test]
    fn allow_with_destructive_risk_asks() {
        let rules = [risky("drop_db", ToolPolicy::Allow, RiskTier::Destructive)];
        assert_eq!(effective_tool_policy(&rules, "drop_db"), ToolPolicy::Ask);
    }

    #[test]
    fn allow_with_risk_at_or_below_write_still_runs() {
        for tier in [RiskTier::Read, RiskTier::Write] {
            let rules = [risky("edit_file", ToolPolicy::Allow, tier)];
            assert_eq!(
                effective_tool_policy(&rules, "edit_file"),
                ToolPolicy::Allow,
                "{tier:?} must not add a prompt"
            );
        }
    }

    #[test]
    fn deny_still_wins_over_a_low_risk() {
        let rules = [risky("bash", ToolPolicy::Deny, RiskTier::Read)];
        assert_eq!(effective_tool_policy(&rules, "bash"), ToolPolicy::Deny);
    }

    /// The #1599 detour: a wildcard `allow` cannot carry a destructive tool
    /// past the gate, and a narrower `allow` without `risk:` does not shed it.
    #[test]
    fn wildcard_allow_with_destructive_risk_asks() {
        let rules = [
            risky("mcp__browser__*", ToolPolicy::Allow, RiskTier::Destructive),
            rule("mcp__browser__evaluate", ToolPolicy::Allow),
        ];
        for name in ["mcp__browser__navigate", "mcp__browser__evaluate"] {
            assert_eq!(
                effective_tool_policy(&rules, name),
                ToolPolicy::Ask,
                "{name}"
            );
        }
    }

    /// An exemption only fills in for a missing rule; a risk-declaring rule
    /// is explicit, so it lifts an exempt tool too.
    #[test]
    fn declared_risk_lifts_an_exempt_tool() {
        let rules = [risky("recall", ToolPolicy::Allow, RiskTier::Privileged)];
        assert_eq!(effective_tool_policy(&rules, "recall"), ToolPolicy::Ask);
    }
}

#[cfg(test)]
mod approver_tests {
    use super::TaskRunner;
    use std::sync::Arc;

    fn runner() -> Arc<TaskRunner> {
        Arc::new(TaskRunner::new_stub_echo())
    }

    /// The fix: a caller that cannot approve gets the denial immediately,
    /// instead of after `hitl.timeout_secs` of silence.
    #[test]
    fn a_caller_that_cannot_approve_is_denied_at_once() {
        let d = super::decide_without_asking(Some(false), "remember").expect("must short-circuit");
        assert!(!d.allow);
        let why = d.reason.unwrap_or_default();
        assert!(why.contains("remember"), "must name the tool: {why}");
        assert!(
            why.contains("murmur"),
            "must name where it can be approved: {why}"
        );
        assert!(
            why.contains("tool-allow"),
            "must name the other way out: {why}"
        );
    }

    /// Controls. An interactive caller must still be asked, and no routed entry
    /// must still reach the existing no-sink handling — this shortcut is only
    /// for a caller that told us it cannot answer.
    #[test]
    fn everyone_else_is_still_asked() {
        assert!(super::decide_without_asking(Some(true), "remember").is_none());
        assert!(super::decide_without_asking(None, "remember").is_none());
    }

    /// A caller that declared it cannot approve must be distinguishable from
    /// one that can, at the map the gate reads. Before this the two were the
    /// same entry and the gate waited on both.
    #[tokio::test]
    async fn the_gate_can_tell_the_two_callers_apart() {
        let r = runner();
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        r.register_client_notifier("t-interactive", tx.clone(), true)
            .await;
        r.register_client_notifier("t-oneshot", tx, false).await;

        let map = r.client_notifiers.lock().await;
        assert_eq!(map.get("t-interactive").map(|(_, ok)| *ok), Some(true));
        assert_eq!(map.get("t-oneshot").map(|(_, ok)| *ok), Some(false));
    }

    /// Step events still reach a caller that cannot approve — it is not a
    /// second-class client, it just has nobody to answer a prompt. If this
    /// breaks, `mur agent send` goes silent about tool progress too.
    #[tokio::test]
    async fn a_non_approving_caller_still_receives_notifications() {
        let r = runner();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        r.register_client_notifier("t1", tx, false).await;

        let sink = {
            let map = r.client_notifiers.lock().await;
            map.get("t1").map(|(tx, _)| tx.clone())
        };
        sink.expect("sink must still be routed")
            .send(serde_json::json!({"method": "step/started"}))
            .await
            .expect("send must reach the client");
        assert!(rx.recv().await.is_some());
    }

    /// `step/tokens` counts the result as it enters history (post-hook), goes
    /// to the watching client, and skips a withdrawn call that never got a
    /// step card.
    #[tokio::test]
    async fn step_tokens_counts_the_post_hook_result_per_started_step() {
        let r = runner();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        r.register_client_notifier("t1", tx, true).await;
        let entry = |content: &str| crate::llm::ToolResultEntry {
            call_id: "c".into(),
            content: content.into(),
            is_error: false,
            status: Default::default(),
            images: Vec::new(),
        };
        let note = "[Large output compressed; original stored.]";
        r.emit_step_tokens(
            "t1",
            &[Some("s1".into()), None],
            &[entry(note), entry("withdrawn")],
        )
        .await;

        let n = rx.recv().await.expect("one frame for the started step");
        assert_eq!(n["method"], "step/tokens");
        assert_eq!(n["params"]["step_id"], "s1");
        assert_eq!(n["params"]["task_id"], "t1");
        let tokens = n["params"]["tokens"].as_u64().expect("a count");
        assert!(tokens > 0 && tokens < note.len() as u64, "{tokens}");
        assert!(rx.try_recv().is_err(), "no frame for the withdrawn call");
    }

    /// Unregistering clears both halves.
    #[tokio::test]
    async fn unregister_removes_the_entry() {
        let r = runner();
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        r.register_client_notifier("t1", tx, false).await;
        r.unregister_client_notifier("t1").await;
        assert!(r.client_notifiers.lock().await.get("t1").is_none());
    }
}
