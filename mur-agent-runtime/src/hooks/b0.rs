//! `B0SafetyHook` — consumer-safe baseline (roadmap §6.1).
//!
//! Implements text rules 1, 2, 3, 4, 5, 7, 8, 11 (M7) plus multimodal
//! rules 13-22 (M3 / M4). Rules 6, 9, 10, 12 ship outside this hook
//! (MCP-install CLI, telemetry redaction, UX docs, companion subsystem
//! respectively). See `docs/cookbook/b0-text-rules.md` for the full
//! map and the §6.1 acceptance footer for ship status.
//!
//! Wiring:
//! * Rules 6 (M9.3) and 11 (M7.7) are **admission control**, not hooks —
//!   see [`verify_mcp_supply_chain`], called by the supervisor before the
//!   agent comes up. They used to live in `on_startup`, which is an
//!   observe-only phase that discards every hook error into a warning, so
//!   neither rule could actually refuse a startup (#791).
//! * `on_prompt_submit` — Rule 3 (M7.4) wraps every prior `role:tool`
//!   message with `<untrusted_tool_result source="tool_result:<name>">`
//!   AND M3.8 reads the multimodal provenance ledger to wrap PDF/OCR
//!   text into `<untrusted_pdf_text>` / `<untrusted_image_text>` /
//!   `<untrusted_share>` (Track C3 send-from-any-app) plus the
//!   `after_untrusted_input` turn-flag.
//! * `pre_tool_use` — most-specific gate first, then situational:
//!   1. Rule 4 (M3.8) — turn-flag set + side-effect tool → AskUser.
//!   2. Rule 1 (M7.1) — fs.write/delete/append/create outside
//!      `agent_home` → AskUser.
//!   3. Rule 5 (M7.2) — process.spawn / shell with
//!      `entitlements.processes.spawn.mode != Any` → Deny if argv[0]
//!      not in allowed[].
//!   4. Rule 2 (M7.3) — network.* with `outbound.mode == Off` → Deny;
//!      `Restricted` + host not in allow_hosts → GrantStore lookup,
//!      then AskUser on first use.
//! * `on_message_send` — Rule 7 (M7.5) scans `view.body` for ~11
//!   credential regex patterns; on hit returns
//!   `MessagePatch::drop_with(reason)` naming the matched class.
//! * `post_tool_use` — Rule 8 (M7.6) for `memory.*` tools redacts
//!   email / SSN / credit-card / phone in the result before
//!   persistence (returns `PostToolUsePatch.replace_output`).

use std::path::PathBuf;
use std::sync::Mutex;

use tokio_util::sync::CancellationToken;

use crate::hooks::{
    AskDefault, Decision, Hook, HookCtx, HookError, MessagePatch, OutboundView, PostToolUsePatch,
    PromptPatch, PromptView, ToolCall, ToolResult, UntrustedWrapper,
};
use mur_common::AgentProfile;
use mur_common::multimodal::ProvenanceLedger;
use mur_common::permissions::{GrantDecision, GrantStore, ScopeKey};

/// Turn-flag raised by `on_prompt_submit` when at least one untrusted
/// multimodal artifact was attached to the current turn. Read by
/// `pre_tool_use` to decide whether to gate side-effect tools.
const TURN_FLAG_AFTER_UNTRUSTED: &str = "after_untrusted_input";

/// B0 rules 11 (M7.7) and 6 (M9.3) — MCP supply-chain admission control.
///
/// Refuses to let the agent come up when an MCP binary is unsigned, or when a
/// binary pinned at install time has changed underneath the profile. Returns
/// the operator-facing reason; the caller must propagate it, not log it.
///
/// **Why this is a free function and not a hook.** Both rules were written to
/// `return Err(...)` from `B0SafetyHook::on_startup` with the documented intent
/// of refusing startup. `HookChain::on_startup` is an observe-only phase: it
/// returns `()` and folds every hook error into `warn!`. Neither rule had ever
/// stopped anything — an MCP binary swapped after install started normally,
/// leaving one warning line in `stderr.log` (#791). Enforcement belongs on a
/// path whose failure aborts, so it lives here and the supervisor calls it with
/// `?` before the runtime is assembled.
///
/// Soft failures (binary missing or unreadable) stay warnings: rule 11 already
/// catches real tampering, and a missing binary is far more likely to be "the
/// user uninstalled the MCP without removing the profile entry" than an attack.
///
/// Description-hash verification is deliberately not done here — it happens
/// lazily at the first MCP call (M9.3.5), because spawning every pinned MCP at
/// startup just to read `tools/list` would N×-bloat boot time.
pub fn verify_mcp_supply_chain(
    mcp_server_binaries: &[PathBuf],
    profile: &AgentProfile,
) -> Result<(), String> {
    // ── Rule 11 (M7.7): signature check. macOS/Windows only; the Linux
    // `verify_signed` helper is a no-op per spec §6.1 row 11.
    for path in mcp_server_binaries {
        if let Err(reason) = crate::hooks::b0_helpers::verify_signed(path) {
            return Err(format!(
                "B0 rule 11: MCP binary signature check failed: {reason}"
            ));
        }
    }

    // ── Rule 6 (M9.3): install-time pin verification.
    for entry in &profile.mcp_servers {
        // A vendored package is the one interpreter-launched shape that CAN be
        // verified: MUR installed it into a directory it owns, so the lockfile
        // recorded at approval still describes the tree unless something
        // changed it. Checked before the interpreter skip below, which would
        // otherwise wave through the very case made verifiable on purpose.
        if let Some(pkg) = &entry.package {
            let lock = pkg.lockfile_path();
            match crate::hooks::b0_helpers::binary_sha256(&lock) {
                Ok(actual) if actual.eq_ignore_ascii_case(&pkg.lockfile_sha256) => {
                    tracing::debug!(mcp = %entry.name, "B0 rule 6: vendored package verified");
                }
                Ok(_) => {
                    return Err(format!(
                        "B0 rule 6: vendored MCP `{}` ({}@{}) no longer matches the tree \
                         installed at approval time — re-run `mur agent mcp vendor <agent> {}` \
                         to reinstall and re-approve, or `mur agent mcp remove <agent> {}` \
                         to uninstall.",
                        entry.name, pkg.name, pkg.version, entry.name, entry.name,
                    ));
                }
                Err(reason) => {
                    // Install gone or unreadable. Same call as a missing binary
                    // below: far more likely "the user deleted it" than an
                    // attack, and refusing to boot would strand the agent with
                    // no way back in.
                    tracing::warn!(
                        mcp = %entry.name,
                        reason = %reason,
                        "B0 rule 6: vendored package lockfile unreadable; continuing",
                    );
                }
            }
            continue;
        }
        let Some(expected) = &entry.binary_sha256 else {
            // Pre-M9 entry — skip silently; the cookbook documents
            // `mur agent mcp pin <name>` as the migration verb.
            continue;
        };
        // An interpreter-launched server (`npx @scope/pkg`, `python -m …`) pins
        // the *interpreter*, not the server. Refusing startup on that hash
        // would brick the agent on any unrelated Node/Python upgrade while
        // still not covering the code it runs, so the pin is reported as
        // unprotected rather than enforced. Package-level pinning is the real
        // fix and a different mechanism.
        if mur_common::exec::is_interpreter_command(&entry.command) {
            tracing::warn!(
                mcp = %entry.name,
                command = %entry.command,
                "B0 rule 6: interpreter-launched MCP — binary pin does not cover the server code, \
                 not enforced (see `mur doctor`)",
            );
            continue;
        }
        // Resolve via PATH exactly as install does (mur_common::exec) so both
        // passes hash the same binary `Command::new` will spawn. A bare
        // `node`/`npx` opened verbatim is a CWD-relative path that doesn't
        // exist, which previously soft-failed and skipped the pin entirely.
        let prog = entry
            .command
            .split_whitespace()
            .next()
            .unwrap_or(&entry.command);
        let path = match mur_common::exec::resolve_command(prog) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    mcp = %entry.name,
                    error = %e,
                    "B0 rule 6: pin verify soft-failed (command not resolvable); continuing",
                );
                continue;
            }
        };
        match crate::hooks::b0_helpers::verify_mcp_binary_hash(expected, &path) {
            Ok(()) => tracing::debug!(mcp = %entry.name, "B0 rule 6: pin verified"),
            Err(crate::hooks::b0_helpers::PinDriftReason::BinaryDrift { .. }) => {
                return Err(format!(
                    "B0 rule 6: MCP `{}` changed since install — \
                     run `mur agent mcp inspect {}` to review the \
                     drift and `mur agent mcp pin {}` to re-approve, \
                     or `mur agent mcp remove {}` to uninstall.",
                    entry.name, entry.name, entry.name, entry.name,
                ));
            }
            Err(soft @ crate::hooks::b0_helpers::PinDriftReason::BinaryMissing { .. })
            | Err(soft @ crate::hooks::b0_helpers::PinDriftReason::BinaryReadError { .. }) => {
                tracing::warn!(
                    mcp = %entry.name,
                    reason = %soft,
                    "B0 rule 6: pin verify soft-failed; continuing",
                );
            }
        }
    }
    Ok(())
}

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

    /// Startup observation only. The two supply-chain rules that used to live
    /// here (6 and 11) are enforced by [`verify_mcp_supply_chain`] instead —
    /// this phase discards hook errors into warnings, so they could never
    /// refuse a startup from here (#791).
    async fn on_startup(
        &self,
        _ctx: &HookCtx,
        _profile: &AgentProfile,
        _tok: &CancellationToken,
    ) -> Result<(), HookError> {
        // ── B1 sandbox attestation ────────────────────────────────────────
        match crate::sandbox::last_status() {
            Some(status) if status.enforcing => {
                tracing::info!(
                    platform = %status.platform,
                    effective_abi = ?status.effective_abi,
                    "B1 kernel sandbox: ENFORCING"
                );
            }
            Some(status) => {
                tracing::warn!(
                    platform = %status.platform,
                    "B1 kernel sandbox: NOT enforcing (advisory-only; upgrade kernel or check permissions)"
                );
            }
            None => {
                tracing::warn!(
                    "B1 kernel sandbox: not applied (sandbox::apply() not called before on_startup)"
                );
            }
        }

        Ok(())
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
            // Heuristic: PDF entries START with the "--- page" marker
            // (M3.4.2 prepends per page). Track C3 share-channel entries
            // START with "--- share" (M-c3.0.2's `process_share_text`
            // prefixes that marker). Anchor with `starts_with` so an
            // OCR'd image whose body happens to include either substring
            // isn't misclassified.
            let tag = if content.starts_with("--- page") {
                "untrusted_pdf_text"
            } else if content.starts_with("--- share") {
                "untrusted_share"
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
            "bash",
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
                mur_common::agent::SpawnMode::Allowlist if call.name() == "bash" => {
                    // `bash`'s input has no argv array to check per-binary
                    // against `spawn.allowed` (it's a free-form shell
                    // command string) — only the coarse gate applies here.
                    // Per-binary enforcement for `bash` lives in the OS
                    // Seatbelt / sandbox layer (see
                    // `mur-agent-runtime/src/sandbox/{policy,macos}.rs`),
                    // not this hook.
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
                mur_common::agent::SpawnMode::Strict if call.name() == "bash" => {
                    // Same free-form-command caveat as `Allowlist`: `bash`'s
                    // input has no argv array to check per-binary against
                    // `spawn.allowed`. Strict mode's actual guarantee (the
                    // resolved shell binary is auto-seeded into
                    // `spawn_allowed_paths`, and system paths are NOT
                    // exempted) is enforced entirely in the OS Seatbelt /
                    // sandbox layer (see
                    // `mur-agent-runtime/src/sandbox/{policy,macos}.rs`),
                    // not this coarse hook. Unlike `Allowlist`, this arm
                    // has no system-path pass-through to omit -- this hook
                    // never granted one for either mode; the only
                    // difference between the two lives in the SBPL
                    // emission (`macos.rs`).
                }
                mur_common::agent::SpawnMode::Strict => {
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
                                "process spawn `{}` is not in the agent's allowlist \
                                 (mode=strict); enable with `mur agent perm \
                                 allow-spawn <name> \"{}\"` (or set spawn.mode to \
                                 `any` for unrestricted)",
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
                // ProxyOnly shares Restricted's host-governance here (gate on
                // allow_hosts via the GrantStore): it only differs from
                // Restricted at the SBPL/port layer (general TCP denied),
                // which this per-tool HITL gate doesn't touch.
                mur_common::agent::NetworkOutboundMode::Restricted
                | mur_common::agent::NetworkOutboundMode::ProxyOnly => {
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
        // value lands in the persisted memory store. The returned patch is
        // consumed by `TaskRunner::apply_post_tool_use` in the agentic loop,
        // which rewrites `ToolResult.output` (folded with CompressHook).
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
    use crate::hooks::{HookCtx, ToolCall};
    use mur_common::agent::{
        Entitlements, FilesystemEntitlement, InboundNetwork, LimitsEntitlement, NetworkEntitlement,
        NetworkOutboundMode, OutboundNetwork, ProcessesEntitlement, ResolveDnsConfig,
        SpawnEntitlement, SpawnMode, SyscallsEntitlement,
    };
    use tempfile::TempDir;

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

    fn ent_with_spawn(mode: SpawnMode, allowed: Vec<String>) -> Entitlements {
        Entitlements {
            network: NetworkEntitlement {
                inbound: InboundNetwork { ports: vec![] },
                outbound: OutboundNetwork {
                    mode: NetworkOutboundMode::Restricted,
                    allow_hosts: vec![],
                    protocols: vec!["tcp".into()],
                    resolve_dns: ResolveDnsConfig::default(),
                },
            },
            filesystem: FilesystemEntitlement::default(),
            processes: ProcessesEntitlement {
                spawn: SpawnEntitlement {
                    mode,
                    allowed,
                    allowed_dirs: vec![],
                },
            },
            syscalls: SyscallsEntitlement::default(),
            limits: LimitsEntitlement::default(),
            llm: Default::default(),
            tools: vec![],
            fail_closed_on_sandbox_error: true,
        }
    }

    // ── Rule 5 coarse gate for `bash` (no argv array to check) ───────────
    // `bash` never matched `SPAWN_TOOLS` before this slice, so it fell
    // through to Allow unconditionally regardless of entitlements. These
    // regression tests pin the fix: `bash` is now gated on `spawn.mode`
    // at this layer, with per-binary enforcement left to the OS sandbox.

    #[tokio::test]
    async fn bash_denied_when_spawn_mode_none() {
        let dir = TempDir::new().unwrap();
        let hook = B0SafetyHook::new();
        let ctx = HookCtx::for_test_with_entitlements(
            dir.path().to_path_buf(),
            1,
            ent_with_spawn(SpawnMode::None, vec![]),
        );
        let call = ToolCall::test("bash", serde_json::json!({"command": "echo hi"}));
        let cancel = CancellationToken::new();
        let decision = hook.pre_tool_use(&ctx, &call, &cancel).await.unwrap();
        assert!(
            matches!(decision, Decision::Deny { .. }),
            "got {decision:?}"
        );
    }

    #[tokio::test]
    async fn bash_allowed_when_spawn_mode_allowlist() {
        let dir = TempDir::new().unwrap();
        let hook = B0SafetyHook::new();
        let ctx = HookCtx::for_test_with_entitlements(
            dir.path().to_path_buf(),
            1,
            ent_with_spawn(SpawnMode::Allowlist, vec![]),
        );
        let call = ToolCall::test("bash", serde_json::json!({"command": "echo hi"}));
        let cancel = CancellationToken::new();
        let decision = hook.pre_tool_use(&ctx, &call, &cancel).await.unwrap();
        assert!(matches!(decision, Decision::Allow), "got {decision:?}");
    }

    #[tokio::test]
    async fn bash_allowed_when_spawn_mode_any() {
        let dir = TempDir::new().unwrap();
        let hook = B0SafetyHook::new();
        let ctx = HookCtx::for_test_with_entitlements(
            dir.path().to_path_buf(),
            1,
            ent_with_spawn(SpawnMode::Any, vec![]),
        );
        let call = ToolCall::test("bash", serde_json::json!({"command": "echo hi"}));
        let cancel = CancellationToken::new();
        let decision = hook.pre_tool_use(&ctx, &call, &cancel).await.unwrap();
        assert!(matches!(decision, Decision::Allow), "got {decision:?}");
    }
}
