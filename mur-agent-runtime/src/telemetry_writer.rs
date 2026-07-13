//! OTel GenAI + mur.* JSONL writer, with notification side-channel
//! so the stdio/socket transport can stream notifications to callers.

use mur_common::telemetry::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};

const CHANNEL_BUF: usize = 256;

/// Internal sequencer message — writer task receives this enum so
/// `Event` and `Flush` requests share a single FIFO channel. Using
/// the same channel guarantees a `Flush` ack only fires AFTER every
/// `Event` queued before it has been written and forwarded.
enum WriterMessage {
    Event(Event),
    Flush(oneshot::Sender<()>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillOutcome {
    Success,
    Failure,
    /// Skill fired but outcome unknown (M5a fallback when per-skill
    /// success/failure is not tracked by the M2 wiring).
    NotEvaluated,
}

#[derive(Debug, Clone)]
pub enum Event {
    LlmCall {
        trace_id: String,
        task_id: String,
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        latency_ms: u64,
        cost_usd: f64,
        provider: String,
        /// Skill names triggered on this turn (M2 deferred).
        fired_skills: Vec<String>,
    },
    ToolCall {
        trace_id: String,
        task_id: String,
        mcp_server: String,
        tool: String,
        duration_ms: u64,
        ok: bool,
    },
    Error {
        kind: String,
        message: String,
        task_id: Option<String>,
        recoverable: bool,
    },
    Warning {
        kind: String,
        message: String,
    },
    Heartbeat {
        uptime_s: u64,
        mem_mb: u64,
        active_tasks: u32,
    },
    TaskProgress {
        task_id: String,
        stage: String,
        message: Option<String>,
        percent: Option<u8>,
    },
    /// Per-skill execution telemetry. Emitted for each skill fired on a
    /// turn. Outcome is `NotEvaluated` when the M2 wiring knows a skill was
    /// triggered but does not track per-skill success/failure (M5a fallback).
    SkillExecuted {
        trace_id: String,
        task_id: String,
        skill_name: String,
        skill_version: String,
        manifest_digest: String,
        outcome: SkillOutcome,
        duration_ms: u64,
    },
    /// Generic hook fire event. `method` is the JSON-RPC notification
    /// method (typically `METHOD_HOOK_FIRED`); `attrs` is the
    /// pre-shaped attribute payload the hook itself produced. The
    /// agent_name / agent_uuid / ts envelope is added by
    /// `event_to_notification`.
    HookFired {
        method: String,
        attrs: Value,
    },
    /// 30 s liveness beat emitted by `BridgeBeacon` on bridge agents
    /// (`entitlements.llm.mode = off`). Routed to peers via the same
    /// notification side-channel as `Heartbeat`; consumed by
    /// `mur agent doctor`'s `bridges:` section through
    /// `bridge::beacon::bridge_status_for_peer` (which inspects
    /// `running.lock` mtime — refreshed each time the writer task
    /// appends a JSONL line).
    BridgeAlive {
        bridge_id: String,
    },
    /// Emitted when a skill is indexed into the LanceDB skill embedding store
    /// (on install or reindex-vec).
    SkillIndexed {
        skill_name: String,
        skill_version: String,
        dims: usize,
        duration_ms: u64,
    },
    /// Emitted for non-literal skill step resolutions at inject time (M6b).
    /// Literal resolutions (default path) are skipped to avoid telemetry flood.
    SkillStepResolved {
        skill_name: String,
        step_index: usize,
        intent: Option<String>,
        picked_tool: Option<String>,
        source: String,
    },
    /// Emitted once per routed turn by `FallbackLlmClient::generate` after the
    /// fallback/cascade loop resolves (Ok or final Err). Best-effort — never
    /// blocks or fails the turn. Training-data feed for the future memory
    /// router (Intelligent Switching Phase B).
    Routing {
        agent: String,
        task_id: Option<String>,
        /// e.g. "interactive" / "background/scheduled" (see `intent_label`).
        intent: String,
        /// The model_ref that ultimately served (or was last attempted on
        /// final failure).
        model_ref: String,
        /// Which mechanism chose the candidate list, e.g.
        /// "smart-background" / "explicit" / "fallback-advance".
        reason: String,
        /// "ok" | "escalated" | "structural_fail" | "error".
        outcome: String,
        attempts: u32,
        escalations: u32,
        input_tokens: u64,
        output_tokens: u64,
        /// First user message text, truncated (see `ROUTING_SUMMARY_MAX` in
        /// `llm::fallback`).
        task_summary: String,
    },
}

pub struct TelemetryWriter {
    tx: mpsc::Sender<WriterMessage>,
    /// Cloned-out `Event`-only sender for callers (hook chain) that
    /// don't need to drive `flush`. Built once by `sender()` so the
    /// adapter task is shared across all clones.
    event_tx: mpsc::Sender<Event>,
}

impl TelemetryWriter {
    /// Spawn the background writer task. Returns a handle that submits events
    /// plus a receiver that downstream transports subscribe to for live
    /// notification forwarding.
    pub async fn new(
        dir: PathBuf,
        agent_name: String,
        agent_uuid: String,
    ) -> std::io::Result<(Self, mpsc::Receiver<Value>)> {
        fs::create_dir_all(&dir).await?;
        let (in_tx, mut in_rx) = mpsc::channel::<WriterMessage>(CHANNEL_BUF);
        let (out_tx, out_rx) = mpsc::channel::<Value>(CHANNEL_BUF);
        tokio::spawn(async move {
            while let Some(msg) = in_rx.recv().await {
                match msg {
                    WriterMessage::Event(ev) => {
                        let mut notif = event_to_notification(&ev, &agent_name, &agent_uuid);
                        // ── B0 rule 9 (M8.1): redact every string leaf in the
                        // notification envelope before it lands on disk OR is
                        // forwarded to a transport subscriber. Single chokepoint:
                        // any new Event variant or hook attribute inherits this
                        // automatically.
                        redact_envelope(&mut notif);
                        let today = chrono::Utc::now().format("%Y-%m-%d");
                        let path = dir.join(format!("{today}.jsonl"));
                        if let Ok(mut f) = OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&path)
                            .await
                        {
                            let line = format!(
                                "{}\n",
                                serde_json::to_string(&notif["params"]).unwrap_or_default()
                            );
                            let _ = f.write_all(line.as_bytes()).await;
                        }
                        let _ = out_tx.send(notif).await;
                    }
                    WriterMessage::Flush(ack) => {
                        // FIFO-ordered: every Event queued before this
                        // Flush has finished its write+send by the time
                        // we get here. Ack the caller; ignore drop on
                        // the rx side (caller went away mid-flush).
                        let _ = ack.send(());
                    }
                }
            }
        });

        // Adapter so `sender()` can hand out an `mpsc::Sender<Event>`
        // for callers that don't need flush semantics. Forwarding
        // task stays alive as long as the inner channel does.
        let (ev_tx, mut ev_rx) = mpsc::channel::<Event>(CHANNEL_BUF);
        let in_tx_for_adapter = in_tx.clone();
        tokio::spawn(async move {
            while let Some(ev) = ev_rx.recv().await {
                if in_tx_for_adapter
                    .send(WriterMessage::Event(ev))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        Ok((
            Self {
                tx: in_tx,
                event_tx: ev_tx,
            },
            out_rx,
        ))
    }

    pub async fn emit(&self, ev: Event) {
        let _ = self.tx.send(WriterMessage::Event(ev)).await;
    }

    /// Returns a clonable sender for use as a `TelemetryEmitter` (hook
    /// chain). Sending fails silently if the writer task has stopped.
    pub fn sender(&self) -> mpsc::Sender<Event> {
        self.event_tx.clone()
    }

    /// Wait for every prior event to be written + forwarded. Sends a
    /// FIFO-ordered marker through the same channel events flow on,
    /// then awaits the writer task's ack — guarantees every preceding
    /// `emit()` has finished its disk write by the time `flush()`
    /// returns. (Was a 250 ms `sleep`; flaked under macOS / Windows
    /// CI fs latency, so we drain instead.)
    pub async fn flush(&self) {
        let (ack_tx, ack_rx) = oneshot::channel();
        if self.tx.send(WriterMessage::Flush(ack_tx)).await.is_err() {
            // Writer task has exited — nothing left to flush.
            return;
        }
        let _ = ack_rx.await;
    }
}

fn event_to_notification(ev: &Event, name: &str, uuid: &str) -> Value {
    let mut params = json!({
        MUR_AGENT_NAME: name,
        MUR_AGENT_UUID: uuid,
        "ts": chrono::Utc::now().to_rfc3339(),
    });
    let method = match ev {
        Event::LlmCall {
            trace_id,
            task_id,
            model,
            input_tokens,
            output_tokens,
            latency_ms,
            cost_usd,
            provider,
            fired_skills,
        } => {
            params[GEN_AI_PROVIDER_NAME] = json!(provider);
            params[GEN_AI_REQUEST_MODEL] = json!(model);
            params[GEN_AI_USAGE_INPUT_TOKENS] = json!(input_tokens);
            params[GEN_AI_USAGE_OUTPUT_TOKENS] = json!(output_tokens);
            params["latency_ms"] = json!(latency_ms);
            params["cost_usd"] = json!(cost_usd);
            params["trace_id"] = json!(trace_id);
            params[MUR_TASK_ID] = json!(task_id);
            if !fired_skills.is_empty() {
                params[MUR_FIRED_SKILLS] = json!(fired_skills);
            }
            METHOD_LLM_CALL
        }
        Event::SkillExecuted {
            trace_id,
            task_id,
            skill_name,
            skill_version,
            manifest_digest,
            outcome,
            duration_ms,
        } => {
            params["trace_id"] = json!(trace_id);
            params[MUR_TASK_ID] = json!(task_id);
            params[MUR_SKILL_NAME] = json!(skill_name);
            params[MUR_SKILL_VERSION] = json!(skill_version);
            params[MUR_SKILL_OUTCOME] = json!(outcome);
            params["duration_ms"] = json!(duration_ms);
            params[MUR_SKILL_MANIFEST_DIGEST] = json!(manifest_digest);
            METHOD_SKILL_EXECUTED
        }
        Event::ToolCall {
            trace_id,
            task_id,
            mcp_server,
            tool,
            duration_ms,
            ok,
        } => {
            params["trace_id"] = json!(trace_id);
            params[MUR_TASK_ID] = json!(task_id);
            params[MUR_MCP_SERVER] = json!(mcp_server);
            params["tool"] = json!(tool);
            params["duration_ms"] = json!(duration_ms);
            params["ok"] = json!(ok);
            METHOD_TOOL_CALL
        }
        Event::Error {
            kind,
            message,
            task_id,
            recoverable,
        } => {
            params["kind"] = json!(kind);
            params["message"] = json!(message);
            params["recoverable"] = json!(recoverable);
            if let Some(t) = task_id {
                params[MUR_TASK_ID] = json!(t);
            }
            METHOD_ERROR
        }
        Event::Warning { kind, message } => {
            params["kind"] = json!(kind);
            params["message"] = json!(message);
            METHOD_WARNING
        }
        Event::Heartbeat {
            uptime_s,
            mem_mb,
            active_tasks,
        } => {
            params["uptime_s"] = json!(uptime_s);
            params["mem_mb"] = json!(mem_mb);
            params["active_tasks"] = json!(active_tasks);
            METHOD_HEARTBEAT
        }
        Event::TaskProgress {
            task_id,
            stage,
            message,
            percent,
        } => {
            params[MUR_TASK_ID] = json!(task_id);
            params["stage"] = json!(stage);
            if let Some(m) = message {
                params["message"] = json!(m);
            }
            if let Some(p) = percent {
                params["percent"] = json!(p);
            }
            METHOD_TASK_PROGRESS
        }
        Event::HookFired { method, attrs } => {
            // `attrs` is the hook-shaped payload (already includes
            // mur.hook.name, mur.hook.phase, agent identity, etc.); merge
            // its keys onto the envelope and use the requested method.
            if let Value::Object(map) = attrs {
                for (k, v) in map {
                    params[k] = v.clone();
                }
            }
            return json!({"jsonrpc": "2.0", "method": method, "params": params});
        }
        Event::BridgeAlive { bridge_id } => {
            params["bridge_id"] = json!(bridge_id);
            METHOD_BRIDGE_ALIVE
        }
        Event::SkillIndexed {
            skill_name,
            skill_version,
            dims,
            duration_ms,
        } => {
            params[MUR_SKILL_NAME] = json!(skill_name);
            params[MUR_SKILL_VERSION] = json!(skill_version);
            params["dims"] = json!(dims);
            params["duration_ms"] = json!(duration_ms);
            METHOD_SKILL_INDEXED
        }
        Event::SkillStepResolved {
            skill_name,
            step_index,
            intent,
            picked_tool,
            source,
        } => {
            params[MUR_SKILL_NAME] = json!(skill_name);
            params["step_index"] = json!(step_index);
            if let Some(i) = intent {
                params["intent"] = json!(i);
            }
            if let Some(t) = picked_tool {
                params["picked_tool"] = json!(t);
            }
            params["source"] = json!(source);
            METHOD_SKILL_STEP_RESOLVED
        }
        Event::Routing {
            agent,
            task_id,
            intent,
            model_ref,
            reason,
            outcome,
            attempts,
            escalations,
            input_tokens,
            output_tokens,
            task_summary,
        } => {
            params["agent"] = json!(agent);
            if let Some(t) = task_id {
                params[MUR_TASK_ID] = json!(t);
            }
            params["intent"] = json!(intent);
            params["model_ref"] = json!(model_ref);
            params["reason"] = json!(reason);
            params["outcome"] = json!(outcome);
            params["attempts"] = json!(attempts);
            params["escalations"] = json!(escalations);
            params[GEN_AI_USAGE_INPUT_TOKENS] = json!(input_tokens);
            params[GEN_AI_USAGE_OUTPUT_TOKENS] = json!(output_tokens);
            params["task_summary"] = json!(task_summary);
            METHOD_ROUTING
        }
    };
    params[MUR_EVENT_TYPE] = json!(method);
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

/// Adapter from the runtime's `TelemetryWriter` channel to the
/// `hooks::TelemetryEmitter` trait. Constructed once at supervisor
/// startup and shared across the hook chain.
pub struct WriterTelemetryEmitter {
    tx: mpsc::Sender<Event>,
}

impl WriterTelemetryEmitter {
    pub fn new(tx: mpsc::Sender<Event>) -> Self {
        Self { tx }
    }
}

#[async_trait::async_trait]
impl crate::hooks::TelemetryEmitter for WriterTelemetryEmitter {
    async fn emit_span_event(&self, name: &str, attrs: Value) {
        // Best-effort. If the channel is full / closed we silently drop —
        // telemetry must never block the hook chain.
        let _ = self
            .tx
            .send(Event::HookFired {
                method: name.to_string(),
                attrs,
            })
            .await;
    }
}

/// B0 rule 9 (M8.1): walk the JSON envelope and redact every string
/// leaf in place. Applies the secret + home-path redactors in
/// sequence; structural fields (numbers, bools, arrays of numbers)
/// are untouched.
///
/// This is the single chokepoint between the in-memory `Event` and
/// the on-disk JSONL / forwarded notification. Adding a new event
/// variant or new hook attribute inherits this automatically.
pub(crate) fn redact_envelope(value: &mut Value) {
    match value {
        Value::String(s) => {
            let stage1 = crate::hooks::b0_helpers::redact_secrets(s);
            let stage2 = crate::hooks::b0_helpers::redact_home_path(&stage1);
            // Only allocate-and-replace if something actually changed.
            if stage2 != *s {
                *s = stage2.into_owned();
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_envelope(item);
            }
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                redact_envelope(v);
            }
        }
        Value::Bool(_) | Value::Number(_) | Value::Null => {}
    }
}

#[cfg(test)]
mod redact_envelope_tests {
    use super::*;

    #[test]
    fn redacts_string_leaf() {
        let mut v = json!("oops sk-abcd1234567890efghij1234 leaked");
        redact_envelope(&mut v);
        assert!(v.as_str().unwrap().contains("[REDACTED:openai_key]"));
    }

    #[test]
    fn redacts_nested_object() {
        let mut v = json!({
            "tool_input": {"key": "sk-ant-abcdefghijklmnop-9999"},
            "ok": true,
            "tokens": 42,
        });
        redact_envelope(&mut v);
        let s = v["tool_input"]["key"].as_str().unwrap();
        assert!(s.contains("[REDACTED:anthropic_key]"));
        // Numbers + bools untouched.
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["tokens"], json!(42));
    }

    #[test]
    fn redacts_inside_array() {
        let mut v = json!(["clean", "ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "ok"]);
        redact_envelope(&mut v);
        let arr = v.as_array().unwrap();
        assert_eq!(arr[0].as_str().unwrap(), "clean");
        assert!(arr[1].as_str().unwrap().contains("[REDACTED:github_pat]"));
        assert_eq!(arr[2].as_str().unwrap(), "ok");
    }

    #[test]
    fn redacts_home_paths_in_error_message() {
        let mut v = json!({
            "kind": "io_error",
            "message": "failed to read /Users/alice/.ssh/id_rsa",
        });
        redact_envelope(&mut v);
        let m = v["message"].as_str().unwrap();
        assert!(m.contains("~/.ssh/id_rsa"), "got {m:?}");
        assert!(!m.contains("alice"), "got {m:?}");
    }

    #[test]
    fn clean_envelope_unchanged() {
        let mut v = json!({
            "trace_id": "t1",
            "model": "claude-opus-4-7",
            "input_tokens": 100,
            "ok": true,
        });
        let before = v.clone();
        redact_envelope(&mut v);
        assert_eq!(v, before);
    }

    #[test]
    fn redacts_both_secret_and_home_path_in_one_string() {
        let mut v = json!("api_key=abcdefghij1234567890 saved to /home/bob/.env");
        redact_envelope(&mut v);
        let s = v.as_str().unwrap();
        assert!(s.contains("[REDACTED:env_assignment]"), "got {s:?}");
        assert!(s.contains("~/.env"), "got {s:?}");
    }
}

#[cfg(test)]
mod routing_event_tests {
    use super::*;

    #[test]
    fn routing_event_serializes_fields() {
        let ev = Event::Routing {
            agent: "coach".into(),
            task_id: Some("t1".into()),
            intent: "background/scheduled".into(),
            model_ref: "cheap".into(),
            reason: "smart-background".into(),
            outcome: "escalated".into(),
            attempts: 2,
            escalations: 1,
            input_tokens: 10,
            output_tokens: 5,
            task_summary: "do the thing".into(),
        };
        let notif = event_to_notification(&ev, "coach", "uuid-1");
        let line = notif.to_string();
        assert!(line.contains("mur.routing"), "got {line}");
        assert!(line.contains("smart-background"), "got {line}");
        assert!(line.contains("\"outcome\":\"escalated\""), "got {line}");
        assert!(line.contains("\"model_ref\":\"cheap\""), "got {line}");
        assert!(
            line.contains("\"task_summary\":\"do the thing\""),
            "got {line}"
        );
        assert_eq!(notif["method"], "mur.routing");
    }
}
