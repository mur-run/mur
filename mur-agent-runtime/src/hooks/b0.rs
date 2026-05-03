//! `B0SafetyHook` — multimodal rules implementation (M3.8).
//!
//! Currently implements rules 13-22 from roadmap §6.1 (the multimodal
//! consumer-safe baseline). The text-only rules (1-12) wait for M8
//! per the roadmap timeline.
//!
//! Wiring:
//! * `on_prompt_submit` (M3.8.1) reads
//!   `<agent_home>/telemetry/inputs.jsonl` for the current turn, looks
//!   up each entry's `<agent_home>/telemetry/inputs/{sha256}.txt`
//!   sidecar, and builds a `PromptPatch` with one `wrap_untrusted` per
//!   entry plus the `after_untrusted_input` turn-flag.
//! * `pre_tool_use` (M3.8.2) checks for the `after_untrusted_input`
//!   turn-flag and denies side-effect tools (delete / spawn / send /
//!   egress / network / .write / .publish) via `Decision::AskUser`,
//!   per roadmap §4.3 step 9.
//!
//! The remaining roadmap rules (sandbox attestation, GrantStore
//! lookup, post_tool_use redaction, on_message_received untrusted
//! flag) land with the M8 text-only baseline.

use std::path::PathBuf;
use std::sync::Mutex;

use tokio_util::sync::CancellationToken;

use crate::hooks::{
    AskDefault, Decision, Hook, HookCtx, HookError, MessagePatch, OutboundView, PostToolUsePatch,
    PromptPatch, PromptView, ToolCall, ToolResult, UntrustedWrapper,
};
use mur_common::multimodal::ProvenanceLedger;
use mur_common::permissions::{GrantDecision, GrantStore, ScopeKey};

/// Turn-flag raised by `on_prompt_submit` when at least one untrusted
/// multimodal artifact was attached to the current turn. Read by
/// `pre_tool_use` to decide whether to gate side-effect tools.
const TURN_FLAG_AFTER_UNTRUSTED: &str = "after_untrusted_input";

pub struct B0SafetyHook {
    /// Loaded lazily on first use per `HookCtx::agent_home()` so tests
    /// can construct the hook with `B0SafetyHook::new()` and the real
    /// runtime gets it initialized on first tool call. The cached
    /// `(PathBuf, GrantStore)` pair lets us detect agent_home swaps
    /// (e.g. test fixtures sharing a hook) and reload accordingly.
    grant_store: Mutex<Option<(PathBuf, GrantStore)>>,
}

impl B0SafetyHook {
    pub fn new() -> Self {
        Self {
            grant_store: Mutex::new(None),
        }
    }

    /// Load (or return cached) GrantStore for this `agent_home`.
    fn ensure_store(&self, agent_home: &std::path::Path) -> std::io::Result<()> {
        let mut guard = self.grant_store.lock().unwrap();
        let needs_reload = match &*guard {
            Some((cached_home, _)) => cached_home != agent_home,
            None => true,
        };
        if needs_reload {
            let mut store = GrantStore::new(agent_home);
            store.load()?;
            *guard = Some((agent_home.to_path_buf(), store));
        }
        Ok(())
    }

    /// Lookup a grant. Returns `None` if no grant or the grant is
    /// expired. The `now` clock comes from `chrono::Utc::now()` rather
    /// than `HookCtx::clock` because `GrantStore` consumes
    /// `chrono::DateTime<Utc>` directly.
    fn lookup_grant(&self, scope: &ScopeKey) -> Option<GrantDecision> {
        let guard = self.grant_store.lock().unwrap();
        guard
            .as_ref()
            .and_then(|(_, store)| store.lookup(scope, chrono::Utc::now()))
    }
}

impl Default for B0SafetyHook {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Hook for B0SafetyHook {
    fn name(&self) -> &str {
        "B0SafetyHook"
    }

    async fn on_prompt_submit(
        &self,
        ctx: &HookCtx,
        view: &PromptView,
        _tok: &CancellationToken,
    ) -> Result<PromptPatch, HookError> {
        // ── Rule 3 (M7.4): spotlight every prior tool-result message. ────
        // We do NOT modify view.messages here (the trait surface returns
        // PromptPatch.wrap_untrusted; the runtime injects the wrappers
        // into the model's input). Each tool message becomes a separate
        // UntrustedWrapper tagged with `tool_result:<tool_name>`.
        let mut wrappers: Vec<UntrustedWrapper> = Vec::new();
        for msg in view.messages.iter() {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("");
            if role != "tool" {
                continue;
            }
            let tool_name = msg
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let content = msg
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            wrappers.push(UntrustedWrapper {
                tag: "untrusted_tool_result".into(),
                source: format!("tool_result:{tool_name}"),
                content,
            });
        }

        // ── M3.8: multimodal provenance ledger. ──────────────────────────
        // Read this turn's ledger entries. Even if no entries are present,
        // we may still have tool wrappers from Rule 3 above — the
        // turn-flag is only raised when multimodal artifacts exist.
        let agent_home = ctx.agent_home();
        let turn_id = ctx.turn_id();
        let ledger = ProvenanceLedger::new(agent_home.join("telemetry/inputs.jsonl"));
        let entries = ledger
            .read_turn(turn_id)
            .map_err(|e| HookError::Runtime(format!("read_turn: {e}")))?;
        let has_multimodal = !entries.is_empty();

        for e in entries {
            let txt_path = agent_home
                .join("telemetry/inputs")
                .join(format!("{}.txt", e.sha256));
            // M3.8.0 always writes the sidecar (empty file when there's no
            // text). A missing file means we're reading an older ledger
            // pre-M3.8.0 — fall back to an empty wrapper rather than
            // silently dropping the provenance entry, but warn so a
            // forensic reader can spot the gap.
            let content = match std::fs::read_to_string(&txt_path) {
                Ok(c) => c,
                Err(err) => {
                    tracing::warn!(
                        "B0SafetyHook: missing text sidecar at {} ({}); using empty wrapper",
                        txt_path.display(),
                        err,
                    );
                    String::new()
                }
            };
            // Heuristic: PDF entries START with the "--- page" marker that
            // the pipeline (M3.4.2) prepends per page. Anchor with
            // `starts_with` so an OCR'd image whose body happens to
            // include that substring isn't misclassified as a PDF.
            let tag = if content.starts_with("--- page") {
                "untrusted_pdf_text"
            } else {
                "untrusted_image_text"
            };
            wrappers.push(UntrustedWrapper {
                tag: tag.into(),
                source: e.source.clone(),
                content,
            });
        }

        // Nothing to wrap and no flag to raise → noop. Note the
        // turn-flag is gated on `has_multimodal` (NOT `wrappers`) so
        // tool-result-only turns don't trigger Rule 4's
        // after-untrusted-input gate.
        if wrappers.is_empty() && !has_multimodal {
            return Ok(PromptPatch::noop());
        }

        let turn_flags = if has_multimodal {
            vec![TURN_FLAG_AFTER_UNTRUSTED.into()]
        } else {
            Vec::new()
        };

        Ok(PromptPatch {
            wrap_untrusted: wrappers,
            turn_flags,
            ..PromptPatch::noop()
        })
    }

    async fn pre_tool_use(
        &self,
        ctx: &HookCtx,
        call: &ToolCall,
        _tok: &CancellationToken,
    ) -> Result<Decision, HookError> {
        // ── Rule 4 (M3.8): no same-turn side-effect after fresh untrusted input. ─
        // Fires FIRST so the most-specific gate wins: if the runtime
        // already saw a multimodal/card-import wrapper this turn AND the
        // tool is a known side-effect, we ask before doing anything else.
        if ctx
            .turn_flags()
            .iter()
            .any(|f| f == TURN_FLAG_AFTER_UNTRUSTED)
            && is_side_effect_tool(call.name())
        {
            // M3.8.2 only needs the scope key as a stable identifier for
            // the user-facing prompt; the per-input schema-hash dimension
            // belongs to the M8 GrantStore lookup. Tag the key with the
            // rule name so future grants are scoped to
            // "after-untrusted-input + this tool" rather than the tool
            // alone.
            let scope_key = ScopeKey {
                agent_id: ctx.agent_uuid.clone(),
                tool_name: format!("{}::{}", TURN_FLAG_AFTER_UNTRUSTED, call.name()),
                input_schema_hash: String::new(),
            };
            return Ok(Decision::AskUser {
                scope_key,
                prompt: format!(
                    "An attached image or PDF may contain instructions. Allow `{}` to run anyway?",
                    call.name()
                ),
                default: AskDefault::Deny,
            });
        }
        // ── Rule 5: process.spawn / eval / shell deny by default. ────────
        // Per-agent toggle via entitlements.processes.spawn.mode:
        //   - Allowlist (default) + empty allowed[]  → deny everything
        //   - Allowlist + allowed[]                  → only those programs allow
        //   - Any                                    → unrestricted (user opted in)
        //
        // We match the family of tool names — anything that ultimately
        // exec()s a child process. Every match enters the gate; only
        // `Any` mode falls through to subsequent rules.
        const SPAWN_TOOLS: &[&str] = &[
            "process.spawn",
            "process.exec",
            "shell.exec",
            "shell.run",
            "eval.run",
            "command.run",
        ];
        if SPAWN_TOOLS.contains(&call.name()) {
            let spawn = &ctx.entitlements().processes.spawn;
            match spawn.mode {
                mur_common::agent::SpawnMode::Any => {
                    // user opted in; fall through to Rule 1 / default Allow
                }
                mur_common::agent::SpawnMode::None => {
                    return Ok(Decision::Deny {
                        reason: format!(
                            "process spawn is disabled by entitlements (mode=none); \
                             enable with `mur agent perm allow-spawn <name>` (or set \
                             spawn.mode to `allowlist` / `any`). Tool: `{}`",
                            call.name()
                        ),
                    });
                }
                mur_common::agent::SpawnMode::Allowlist => {
                    let argv0 = call
                        .input
                        .get("argv")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.first())
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if !spawn.allowed.iter().any(|p| p == argv0) {
                        return Ok(Decision::Deny {
                            reason: format!(
                                "process spawn `{}` is not in the agent's allowlist; \
                                 enable with `mur agent perm allow-spawn <name> \"{}\"` \
                                 (or set spawn.mode to `any` for unrestricted)",
                                argv0, argv0,
                            ),
                        });
                    }
                }
            }
        }
        // ── Rule 1: FS confinement (advisory). ───────────────────────────
        // For fs.write / fs.delete / fs.append / fs.create on a path
        // outside <agent_home>, ask the user. Read-only access is the
        // OS picker's job (granted via tauri-plugin-fs's runtime open
        // dialog), so we don't gate fs.read here.
        if matches!(
            call.name(),
            "fs.write" | "fs.delete" | "fs.append" | "fs.create"
        ) && let Some(path) = call.input.get("path").and_then(|v| v.as_str())
        {
            let candidate = std::path::Path::new(path);
            if !crate::hooks::b0_helpers::path_confined_to(candidate, ctx.agent_home()) {
                let scope_key = ScopeKey {
                    agent_id: ctx.agent_uuid.clone(),
                    tool_name: format!("fs_outside_home::{}", call.name()),
                    input_schema_hash: String::new(),
                };
                return Ok(Decision::AskUser {
                    scope_key,
                    prompt: format!(
                        "`{}` is about to write at `{}`, which is outside the agent's \
                         home directory. Allow this once?",
                        call.name(),
                        path,
                    ),
                    default: AskDefault::Deny,
                });
            }
        }
        // ── Rule 2 (M7.3): outbound network allowlist + GrantStore. ──────
        // Per-agent toggle via entitlements.network.outbound.mode:
        //   - Unrestricted   → fall through (Allow)
        //   - Off            → deny everything
        //   - Restricted (default) + host on allow_hosts → fall through
        //   - Restricted + host NOT on allow_hosts:
        //       * GrantStore Allow → fall through silently
        //       * GrantStore Deny  → bail with revoke instructions
        //       * no grant         → AskUser (default Deny)
        //
        // We extract the host from the tool input's `url` field — the
        // common shape across the network tool family. Adapt per-tool
        // if a future tool surfaces a different field name.
        const NET_TOOLS: &[&str] = &[
            "network.http_get",
            "network.http_post",
            "network.tcp_connect",
            "network.udp_send",
            "network.dns_resolve",
            "fetch", // common alias
        ];
        if NET_TOOLS.contains(&call.name()) {
            let outbound = &ctx.entitlements().network.outbound;
            let host = call
                .input
                .get("url")
                .and_then(|v| v.as_str())
                .and_then(|u| {
                    url::Url::parse(u)
                        .ok()
                        .and_then(|p| p.host_str().map(|s| s.to_string()))
                });

            match outbound.mode {
                mur_common::agent::NetworkOutboundMode::Unrestricted => {
                    // user opted in; fall through to default Allow
                }
                mur_common::agent::NetworkOutboundMode::Off => {
                    return Ok(Decision::Deny {
                        reason: "outbound network is disabled by entitlements (mode=off)".into(),
                    });
                }
                mur_common::agent::NetworkOutboundMode::Restricted => {
                    let host = host.unwrap_or_default();
                    if !crate::hooks::b0_helpers::host_is_allowlisted(&host, &outbound.allow_hosts)
                    {
                        // Lazy-load the GrantStore for this agent. A load
                        // failure (e.g. corrupt YAML) shouldn't crash
                        // execution — fall through to AskUser so the user
                        // can re-grant.
                        if self.ensure_store(ctx.agent_home()).is_err() {
                            tracing::warn!(
                                "B0SafetyHook: GrantStore load failed; falling back to AskUser"
                            );
                        }
                        let scope_key = ScopeKey {
                            agent_id: ctx.agent_uuid.clone(),
                            tool_name: format!("network_outbound::{host}"),
                            input_schema_hash: String::new(),
                        };
                        match self.lookup_grant(&scope_key) {
                            Some(GrantDecision::Allow) => {
                                // fall through silently
                            }
                            Some(GrantDecision::Deny) => {
                                return Ok(Decision::Deny {
                                    reason: format!(
                                        "outbound to `{host}` was previously denied; \
                                         revoke via `mur agent perm revoke …` to re-prompt"
                                    ),
                                });
                            }
                            None => {
                                return Ok(Decision::AskUser {
                                    scope_key,
                                    prompt: format!(
                                        "Agent wants to make an outbound request to \
                                         `{host}`. This host isn't on the agent's \
                                         allowlist. Allow once?"
                                    ),
                                    default: AskDefault::Deny,
                                });
                            }
                        }
                    }
                }
            }
        }
        Ok(Decision::Allow)
    }

    async fn on_message_send(
        &self,
        _ctx: &HookCtx,
        view: &OutboundView,
        _tok: &CancellationToken,
    ) -> Result<MessagePatch, HookError> {
        // ── Rule 7 (M7.5): outbound credential pre-filter. ───────────────
        // Match the body against ~11 well-known credential patterns
        // (OpenAI / Anthropic / AWS / GitHub / GCP / JWT / PEM /
        // Slack-webhook / .env-style assignment). On hit, drop the
        // message entirely with a reason that names the matched class
        // so the agent can self-correct.
        if let Some(label) = crate::hooks::b0_helpers::scan_for_secrets(&view.body) {
            tracing::warn!("B0SafetyHook: dropping outbound — secret detected ({label})");
            return Ok(MessagePatch::drop_with(&format!(
                "outbound message blocked: contains credential pattern ({label}). \
                 Strip the secret and retry. (B0 rule 7)"
            )));
        }
        Ok(MessagePatch::noop())
    }

    async fn post_tool_use(
        &self,
        _ctx: &HookCtx,
        call: &ToolCall,
        result: &ToolResult,
        _tok: &CancellationToken,
    ) -> Result<PostToolUsePatch, HookError> {
        // ── Rule 8 (M7.6): redact PII in memory.* tool outputs. ──────────
        // Only memory.* tools get the redaction pass; everything else
        // is unchanged. The redactor is permissive (catches obvious
        // email/SSN/cc/phone patterns) and the patch is consumed by
        // the supervisor to rewrite `ToolResult.output` before the
        // value lands in the persisted memory store.
        //
        // NOTE: this hook returns the patch; wiring `chain.post_tool_use`'s
        // returned `PostToolUsePatch` into the persistence path is
        // tracked separately. M7.6 only covers the hook + chain folding;
        // the supervisor caller does not yet act on `replace_output`.
        if !call.name().starts_with("memory.") {
            return Ok(PostToolUsePatch::default());
        }
        let Some(text) = result.output.as_str() else {
            return Ok(PostToolUsePatch::default());
        };
        let redacted = crate::hooks::b0_helpers::redact_pii(text);
        if redacted == text {
            return Ok(PostToolUsePatch::default());
        }
        Ok(PostToolUsePatch {
            replace_output: Some(serde_json::Value::String(redacted)),
        })
    }
}

/// Side-effect tool name classifier. Errs on the safe side: better to
/// ask once than to silently fire a deletion / spawn / egress.
///
/// The match list mirrors roadmap §4.3 step 9 (`delete*`, `spawn*`,
/// `*.send`, `egress*`, `network.*`, `.write`, `.publish`).
fn is_side_effect_tool(name: &str) -> bool {
    let n = name.to_lowercase();
    n.starts_with("delete")
        || n.ends_with("delete")
        || n.starts_with("spawn")
        || n.ends_with("spawn")
        || n.contains(".send")
        || n.starts_with("send")
        || n.starts_with("egress")
        || n.starts_with("network.")
        || n.contains(".write")
        || n.contains(".publish")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_effect_classifier_matches_expected_names() {
        for n in [
            "fs.delete",
            "delete_user",
            "process.spawn",
            "spawn_shell",
            "messaging.send",
            "send_email",
            "egress.http",
            "network.http_get",
            "fs.write",
            "feed.publish",
        ] {
            assert!(is_side_effect_tool(n), "{n} should be side-effect");
        }
        for n in ["fs.read", "search.query", "patterns.list", "config.get"] {
            assert!(!is_side_effect_tool(n), "{n} should be allowed");
        }
    }
}
