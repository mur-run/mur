//! Pre-seal setup: bundled MCP refresh, sandbox grants, egress proxy,
//! secret resolution, and the B1 sandbox seal itself. Everything here must
//! run while `~/.mur` is still reachable; the seal is the last step.

use super::bootstrap::USER_SECRET_RESOLVE_TIMEOUT;
use crate::profile::Profile;
use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

/// What the pre-seal phase hands back to `entrypoint`.
pub(super) struct Sealed {
    pub(super) egress_proxy: Option<crate::sandbox::egress_proxy::EgressProxyHandle>,
    pub(super) secrets: Arc<crate::secrets::SecretVault>,
    pub(super) approval_token: Option<String>,
    pub(super) sandbox_record: Option<mur_common::agent::SandboxRecord>,
}

pub(super) async fn prepare_and_seal(
    profile: &Profile,
    agent_home: &Path,
    mur_home: &Path,
) -> anyhow::Result<Sealed> {
    // 3c. Self-heal MUR's own copy of the MCP server binary. When an enabled
    //     MCP server points at the bundled location (~/.mur/mcp-servers/...),
    //     refresh it from the mur-mcp-server shipped next to `mur` so it tracks
    //     the running version regardless of how mur was installed (brew/cargo/
    //     source). Done BEFORE the sandbox seals (needs write to ~/.mur) and
    //     before the MCP child is spawned. Best-effort: a non-media agent skips
    //     it entirely, and a failure leaves any existing copy in place.
    {
        let bundled = mur_common::exec::bundled_mcp_server_path();
        let uses_bundled = profile.inner.enabled_mcp_servers().iter().any(|e| {
            let c = std::path::Path::new(&e.command);
            // The command may be the absolute bundled path OR the bare binary
            // name (`mur-mcp-server`, e.g. from a capability install) — both
            // resolve to the bundled copy, so both should trigger the refresh.
            c == bundled || c.file_name() == bundled.file_name()
        });
        let mut bundled_refreshed = None;
        if uses_bundled {
            match mur_common::exec::ensure_bundled_mcp_server() {
                Ok(p) => {
                    info!(path = %p.display(), "ensured bundled mcp-server");
                    bundled_refreshed = Some(p);
                }
                Err(e) => warn!(
                    error = %e,
                    "could not refresh bundled mcp-server; using existing copy if present"
                ),
            }
        }

        // Re-pin the MCP servers MUR ships itself, whose trust anchor is "same
        // install as this runtime" rather than a hash recorded weeks ago: an
        // attacker who can swap one has already swapped the runtime. Without
        // this, every `mur` upgrade leaves the profile pinned to the previous
        // binary and B0 rule 6 (enforcing since #791) refuses to start the
        // agent after a routine upgrade. Third-party entries are untouched.
        //
        // Runs whether or not the bundled server was refreshed: a profile can
        // carry a first-party sibling (`mur-research-gateway`) and no
        // `mur-mcp-server` at all — that is exactly the deep-research worker
        // shape that crash-looped.
        match std::env::current_exe().ok().and_then(|e| {
            e.canonicalize()
                .ok()
                .and_then(|c| c.parent().map(Path::to_path_buf))
        }) {
            Some(runtime_dir) => {
                if let Err(e) = crate::mcp_repin::repin_first_party(
                    agent_home,
                    bundled_refreshed.as_deref(),
                    &runtime_dir,
                ) {
                    warn!(error = %e, "could not re-pin first-party MCP binaries");
                }
            }
            None => warn!(
                "could not locate this runtime's own directory; skipped first-party MCP re-pin"
            ),
        }
    }

    // B1: apply OS-level kernel sandbox based on profile entitlements.
    // Called AFTER profile load (needs entitlements) and AFTER grace cleanup.
    // BEFORE telemetry writer (its file I/O must be within sandbox bounds).
    // BEFORE on_startup hooks.
    // On platforms without Landlock/SBPL support, returns enforcing=false — B0 still applies.
    let fail_closed = profile.inner.entitlements.fail_closed_on_sandbox_error;
    // Always allow the agent's own local LLM port through the kernel sandbox.
    let mut extra_ports: Vec<u16> =
        crate::supervisor_runner::local_llm_port(&profile.inner, mur_home)
            .into_iter()
            .collect();
    // Also allow the persisted VLC HTTP port + the shared runtime-state directory
    // so the proactive co-watching WatchScheduler can operate under the enforced
    // sandbox. Gated on co-watching being set up (vlc.json present): a non-media
    // agent keeps its narrower sandbox rather than being widened to a shared,
    // cross-agent state file it never uses. The VLC port (port-based allowlist on
    // both SBPL and Landlock NetPort) is random-but-sticky in vlc.json, so if VLC
    // has never been set up vlc.json is absent at seal time and a restart picks it
    // up once it exists.
    //
    // We grant the WHOLE `~/.mur/runtime` directory (a sibling of agent_home), not
    // the individual files. The scheduler reads vlc.json, prunes snapshots, and
    // persists watch.json via temp-file + rename. Under Landlock a temp create +
    // rename needs directory-level rights (MAKE_REG/REFER) on the *parent* that a
    // per-file grant cannot confer, and path-beneath rules are silently skipped for
    // paths that don't exist at seal time (watch.json.tmp is transient). Granting
    // the existing directory is both necessary and sufficient on Landlock and
    // matches the macOS SBPL subpath grant — so co-watching works on both kernels,
    // not just macOS. (We create the dir first so the Landlock rule sticks.)
    let mut extra_write_paths: Vec<std::path::PathBuf> = Vec::new();
    // Memory federation (P0): the sleep cycle drops signed snapshot requests
    // and flushed outbox signals into `<mur_home>/inbox/…`. That is the ONLY
    // central-store write the runtime is granted — the daemon treats the inbox
    // as an attacker-writable surface and verifies signatures, so this grant
    // widens no trust boundary. Created first so the Landlock rule sticks
    // (same reasoning as the VLC runtime dir below).
    {
        let inbox_dir = mur_home.join("inbox");
        let _ = std::fs::create_dir_all(
            mur_home.join(mur_common::snapshot_request::SNAPSHOT_REQUEST_DIR),
        );
        extra_write_paths.push(inbox_dir);
    }
    if let Some(vlc) = mur_common::media::load_runtime(mur_home) {
        extra_ports.push(vlc.port);
        let runtime_dir = mur_common::media::runtime_dir(mur_home);
        let _ = std::fs::create_dir_all(&runtime_dir);
        extra_write_paths.push(runtime_dir);
        // snapshot_dir normally lives under runtime/, but vlc.json may point it
        // elsewhere — grant it explicitly so snapshot read/prune works regardless.
        extra_write_paths.push(vlc.snapshot_dir.clone());
        tracing::info!(
            vlc_port = vlc.port,
            "B1 sandbox: allowing VLC HTTP port + runtime dir for co-watching"
        );
    }
    // G1: the loopback egress proxy must exist BEFORE the sandbox seals so
    // its listener port can be carved into the kernel profile. Started
    // post-seal (the old order), the ephemeral port is unreachable to
    // sandboxed MCP children and every scoped egress grant is dead on
    // arrival — proven live 2026-07-09 (deep-research workers: zero
    // CONNECTs reached the proxy; standalone gateway fetch worked).
    let egress_proxy =
        if crate::supervisor_runner::profile_needs_egress(&profile.inner.enabled_mcp_servers()) {
            match crate::sandbox::egress_proxy::start_egress_proxy(&profile.inner.name).await {
                Ok(h) => {
                    tracing::info!(addr = %h.addr, "egress proxy started (pre-sandbox)");
                    Some(h)
                }
                Err(e) => {
                    tracing::warn!(
                        "egress proxy failed to start; scoped MCP servers will be unscoped: {e}"
                    );
                    None
                }
            }
        } else {
            None
        };
    // The vault that carries user-handed credentials for this process's whole
    // life: filled just below (pre-seal, because the keychain is unreachable
    // once the sandbox closes), then mutated live by `secret/set`.
    let secrets = Arc::new(crate::secrets::SecretVault::new());
    // Resolve provider secrets while the paths are still reachable.
    //
    // The client that needs them is built at `prepare_runtime` below — AFTER
    // the seal — so a `file:` ref inside `~/.mur/secrets/` (a denied credential
    // path) could not be read, and the caller fell through to a per-agent
    // Keychain lookup. On a real install both were broken at once, each hiding
    // the other (#866). Same ordering the identity keypair already relies on.
    //
    // Best-effort: a secret that will not resolve here is simply not cached,
    // and the later lookup fails exactly as it did before. Warn rather than
    // abort, because an agent with an unreachable secret still starts and its
    // other candidates may work.
    {
        let reg = mur_common::model::ModelRegistry::load_from(&mur_home.join("models.yaml"))
            .unwrap_or_default();
        let mut cached = 0usize;
        for entry in reg.models.values() {
            if let Some(r) = entry.secret.as_ref() {
                match mur_common::secret::cache_before_seal(r) {
                    Ok(()) => cached += 1,
                    Err(e) => warn!(
                        secret = %r,
                        error = %e,
                        "could not resolve a provider secret before sealing; it \
                         will be unreachable if its path is inside the sandbox's \
                         denied credential store"
                    ),
                }
            }
        }
        // Per-agent credentials too. These are NOT in models.yaml — the account
        // name is derived from the agent's name at resolve time
        // (`llm/anthropic.rs::from_agent_credentials`), so an entry with no
        // `secret:` of its own falls through to them. Missing this is why the
        // first cut of the pre-seal fix left that path still broken after an
        // upgrade.
        //
        // Absent credentials are the normal case (most agents use a model
        // secret), so a miss here is silent — only a resolvable one is cached.
        for key in ["ANTHROPIC_API_KEY", "OPENAI_API_KEY"] {
            let r = mur_common::secret::SecretRef::Keychain {
                service: "mur-agent".to_string(),
                account: format!("{}/{}", profile.inner.name, key),
            };
            if mur_common::secret::cache_before_seal(&r).is_ok() {
                cached += 1;
            }
        }

        // User-handed secrets (murmur `/secret`, `mur agent secret set`). The
        // names come from the profile because a keychain cannot be enumerated;
        // the values are resolved here, pre-seal, for the same reason as the
        // provider keys above. A value the vault will not hold (too short) is
        // skipped with a warning rather than failing startup — the CLI enforces
        // the same floor, so this only fires for a hand-written keychain item.
        for name in &profile.inner.secrets {
            let r = mur_common::secret::SecretRef::Keychain {
                service: crate::secrets::KEYCHAIN_SERVICE.to_string(),
                account: format!("{}/{}", profile.inner.name, name),
            };
            // Bounded, on its own thread. A keychain item this binary has not
            // been authorised for makes the OS raise a modal prompt, and the
            // read blocks until somebody clicks it — which on a headless or
            // login-time start is never. Unbounded, that hangs the agent
            // BEFORE the sandbox seals: not a failure, not a degraded start,
            // just a process that never becomes ready. Verified on a real
            // machine, which is the only place it reproduces: the provider
            // keys above never show it because they were authorised long ago.
            //
            // The orphaned thread is deliberate. It is parked in the OS call,
            // owns nothing this process needs, and is the price of not
            // hanging; the user answers the prompt (or does not) and the agent
            // comes up either way, one secret short until the next `/secret`.
            let (tx, rx) = std::sync::mpsc::channel();
            let probe = r.clone();
            std::thread::spawn(move || {
                let _ = tx.send(mur_common::secret::cache_before_seal(&probe));
            });
            match rx.recv_timeout(USER_SECRET_RESOLVE_TIMEOUT) {
                Ok(Ok(())) => {
                    if let Some(v) = r.resolve_preseal_cached() {
                        use secrecy::ExposeSecret;
                        match secrets.set(name, v.expose_secret()) {
                            Ok(()) => cached += 1,
                            Err(e) => warn!(name, error = %e, "secret skipped"),
                        }
                    }
                }
                Ok(Err(e)) => warn!(
                    name,
                    error = %e,
                    "could not resolve a user secret before sealing; the agent \
                     will not have it until it is set again"
                ),
                Err(_) => warn!(
                    name,
                    timeout_secs = USER_SECRET_RESOLVE_TIMEOUT.as_secs(),
                    "keychain did not answer for this secret in time — most likely \
                     an authorisation prompt nobody is there to click. Starting \
                     without it; approve the prompt (Always Allow) and restart, \
                     or set it again from murmur with /secret"
                ),
            }
        }

        if cached > 0 {
            info!(cached, "provider secrets resolved before sandbox seal");
        }
    }

    // The approval token lives in `secrets/`, which the seal below makes
    // unreadable — to this process too. Load (or create) it now. Failure is
    // not fatal: the agent still runs, and every allow is refused with a
    // message that says why, which is the safe direction.
    let approval_token = match mur_common::hitl::approval_token::load_or_create(mur_home) {
        Ok(t) => Some(t),
        Err(e) => {
            warn!(error = %e, "could not load the HITL approval token before sealing; approvals will be refused until restart");
            None
        }
    };

    let loopback_ports: Vec<u16> = egress_proxy.iter().map(|h| h.addr.port()).collect();
    let granted_digest =
        mur_common::agent::filesystem_grants_digest(&profile.inner.entitlements.filesystem);
    let sandbox_record = match crate::sandbox::apply(
        &profile.inner.entitlements,
        agent_home,
        &extra_ports,
        &loopback_ports,
        &extra_write_paths,
    ) {
        Ok(status) => {
            if !status.enforcing && fail_closed {
                anyhow::bail!(
                    "B1 sandbox applied but not enforcing on this platform \
                     (enforcing=false) and fail_closed_on_sandbox_error=true; \
                     refusing to start unconfined. Set \
                     entitlements.fail_closed_on_sandbox_error: false in the \
                     profile to allow advisory-only mode."
                );
            }
            tracing::info!(
                platform = %status.platform,
                effective_abi = ?status.effective_abi,
                enforcing = status.enforcing,
                dropped = status.dropped.len(),
                "B1 sandbox applied"
            );
            Some(mur_common::agent::SandboxRecord {
                enforcing: status.enforcing,
                mode: status.platform,
                granted_digest,
                dropped: status.dropped,
            })
        }
        Err(e) => {
            if fail_closed {
                return Err(e.context(
                    "B1 sandbox::apply failed and fail_closed_on_sandbox_error=true; \
                     refusing to start. Set entitlements.fail_closed_on_sandbox_error: \
                     false in the profile to allow advisory-only mode.",
                ));
            }
            tracing::warn!(error = %e, "B1 sandbox::apply failed; running advisory-only (B0 remains active)");
            // No policy was installed, so there is nothing to have dropped.
            // `enforcing: false` is the finding here, and it subsumes the rest:
            // the agent has MORE access than its profile grants, not less.
            Some(mur_common::agent::SandboxRecord {
                enforcing: false,
                mode: "advisory-only".into(),
                granted_digest,
                dropped: Vec::new(),
            })
        }
    };

    Ok(Sealed {
        egress_proxy,
        secrets,
        approval_token,
        sandbox_record,
    })
}
