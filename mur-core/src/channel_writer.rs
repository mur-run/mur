//! Append channel events signed by the channel's router/owner agent (v3d).
//!
//! The channel's WRITER (the router/owner agent, e.g. the `"mur"` concierge)
//! signs the events it appends so a downstream reader can verify authority
//! before trusting an event (notably the HITL gate verifying a `HitlResponse`).
//!
//! Migration-safe: if the router's identity is unavailable we fall back to an
//! unsigned `append`, so existing channels (and tests that never created an
//! identity) keep working. Enforcement is opt-in via `MUR_CHANNEL_REQUIRE_SIG`;
//! with it set, a write that could not be signed is refused here rather than
//! appended unsigned for the reader to drop.
use std::path::Path;

use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};

/// The channel's router/owner agent; defined beside the verification that
/// resolves System/Human actors to it ([`mur_channel::verify::ROUTER_AGENT`]).
pub use mur_channel::verify::ROUTER_AGENT;

/// Plant a signing identity for `agent` under `home`, so events that agent
/// writes in a test are SIGNED the way they are in production.
///
/// Every fixture that seeds a channel by hand needs this. Without it the temp
/// home has no `<home>/agents/<agent>/identity.key`, `writer_key` returns
/// `None`, and the event is appended UNSIGNED — which passes only because the
/// reader happens to have enforcement off. Under `MUR_CHANNEL_REQUIRE_SIG=1`
/// (how the product is meant to run) the same fixture writes an event that the
/// verifying fold then drops, and the test fails for a reason that has nothing
/// to do with the behaviour it is checking.
///
/// So this is not test sugar: an unsigned fixture is a fixture that does not
/// reproduce the path it claims to test. Note that the actor decides whose key
/// is checked — `ChannelActor::Agent { id }` resolves to that agent, anything
/// else to [`ROUTER_AGENT`] — so plant the identity matching the actor you
/// write as.
#[cfg(test)]
pub(crate) fn plant_identity_for(home: &Path, agent: &str) -> mur_common::identity::AgentIdentity {
    let agent_home = home.join("agents").join(agent);
    std::fs::create_dir_all(&agent_home).expect("create agent home");
    let id = mur_common::identity::AgentIdentity::generate();
    id.save(&agent_home).expect("save identity");
    id
}

/// [`plant_identity_for`] with the router — the common case, since the router
/// signs every workflow/HITL event MUR writes on a channel's behalf.
#[cfg(test)]
pub(crate) fn plant_writer_identity(home: &Path) -> mur_common::identity::AgentIdentity {
    plant_identity_for(home, ROUTER_AGENT)
}

/// How many writes this process has refused, across every channel.
///
/// Layer 1 (the `Err`) travels to the caller and layer 3 (the sidecar)
/// outlives the process — but several call sites discard the `Err`, and
/// nobody reads a sidecar they don't know to look for. This counter exists
/// so a run can *say*, in its own closing summary, that some of its history
/// is missing. A run that refused writes and reported nothing is the failure
/// this whole change exists to remove.
///
/// Process-wide and monotonic: a run never decrements it. Callers snapshot
/// it at the start and compare at the end (`refusals_since`), which is
/// correct even with concurrent runs in one process — the summary then
/// covers the process's refusals during that window, which is still true
/// and still worth printing.
static REFUSALS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A snapshot of the refusal counter, for `refusals_since`.
pub fn refusal_count() -> usize {
    REFUSALS.load(std::sync::atomic::Ordering::Relaxed)
}

/// How many writes were refused since `mark` (from `refusal_count`).
///
/// Saturating on purpose: the counter only grows, but a caller that passes a
/// stale or fabricated mark gets 0 rather than a panic in a reporting path.
pub fn refusals_since(mark: usize) -> usize {
    refusal_count().saturating_sub(mark)
}

/// Read the router agent's current key version from its `profile.yaml`
/// (`identity.key_version`). Returns 0 on any failure (missing profile,
/// malformed YAML), matching the bootstrap key version.
pub(crate) fn read_key_version(agent_home: &Path) -> u32 {
    let profile_path = agent_home.join("profile.yaml");
    let Ok(yaml) = std::fs::read_to_string(&profile_path) else {
        return 0;
    };
    match serde_yaml_ng::from_str::<mur_common::AgentProfile>(&yaml) {
        Ok(profile) => profile.identity.key_version,
        Err(_) => 0,
    }
}

/// The signing capability our parent handed us on stdin, if any.
///
/// Set once, at process start, by `ingest_signing_handoff`. `None` for every
/// ordinary invocation — a `mur` the user runs reads the key from disk like
/// it always has.
static HANDOFF: std::sync::OnceLock<Option<(String, u32, mur_common::identity::AgentIdentity)>> =
    std::sync::OnceLock::new();

/// Read a `SigningHandoff` from stdin when the parent said one is there.
///
/// Called once from `main`, before anything else can touch stdin, so the
/// pipe is drained deterministically rather than by whoever reads first.
///
/// Absence is silent: no announcement means no handoff, which is every
/// ordinary invocation. But an announcement that cannot be honored is a hard
/// error, NOT a fallback to unsigned:
///
/// - The parent set `SIGNING_HANDOFF_ENV` only because it had decided this
///   child must sign. An unparsable payload means the two disagree about the
///   protocol — a version skew, a truncated pipe, an empty write. There is no
///   deployment in which that is the intended state.
/// - It is deliberately NOT gated on `MUR_CHANNEL_REQUIRE_SIG`. That flag
///   answers "do I tolerate unsigned events?"; this is not that question.
///   The parent already answered it by announcing a handoff. Gating a broken
///   protocol on a tolerance flag would use one switch for two unrelated
///   things, and would leave the default configuration silently degraded.
/// - Returning `Err` rather than warning, because this runs BEFORE
///   `tracing_subscriber` is initialized in `main` — a `tracing::warn!` here
///   has no subscriber and is dropped on the floor. That is exactly how the
///   previous version of this failure managed to be invisible.
pub fn ingest_signing_handoff() -> anyhow::Result<()> {
    if std::env::var_os(mur_common::identity::SIGNING_HANDOFF_ENV).is_none() {
        return Ok(());
    }
    let mut line = String::new();
    if let Err(e) = std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut line) {
        anyhow::bail!(
            "signing handoff was announced via {} but stdin could not be read ({e}); \
             refusing to continue unsigned — the parent expected this process to sign",
            mur_common::identity::SIGNING_HANDOFF_ENV
        );
    }
    let h = serde_json::from_str::<mur_common::identity::SigningHandoff>(&line).map_err(|e| {
        anyhow::anyhow!(
            "signing handoff was announced via {} but could not be parsed ({e}); \
             refusing to continue unsigned — this is a handoff protocol mismatch \
             between parent and child, not a missing capability",
            mur_common::identity::SIGNING_HANDOFF_ENV
        )
    })?;
    let id = mur_common::identity::AgentIdentity::from_secret_bytes(&h.secret);
    let _ = HANDOFF.set(Some((h.agent, h.key_version, id)));
    Ok(())
}

/// The handed-over identity, but only for the writer it actually belongs to.
///
/// A child signs as the agent that spawned it and as nothing else — a run
/// that writes on behalf of some other agent falls back to the disk path.
fn handoff_for(router_agent: &str) -> Option<(&'static mur_common::identity::AgentIdentity, u32)> {
    let (agent, kv, id) = HANDOFF.get()?.as_ref()?;
    (agent == router_agent).then_some((id, *kv))
}

/// Leave a durable, readable mark when a write is refused.
///
/// The refusal itself is an `Err` that propagates, but several call sites
/// discard it (`let _ = append_as_writer(..)` in the DAG executor), and
/// `stderr` from a sandboxed child is routinely dropped. A refusal that
/// nobody can see afterwards is indistinguishable from a write that never
/// happened — the exact failure mode this whole change exists to remove.
///
/// Best-effort on purpose: if the sidecar cannot be written we still return
/// the original refusal. Losing the breadcrumb must never turn a fail-closed
/// refusal into something else.
fn record_refusal(home: &Path, router_agent: &str, reason: &str) {
    // Counted BEFORE the write is attempted. The count is what a run's
    // closing summary reports, and a refusal that could not even be
    // written to the sidecar (unwritable home, full disk) is the one most
    // worth surfacing — counting after the write would lose exactly those.
    REFUSALS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = home.join("channels");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let line = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "router_agent": router_agent,
        "pid": std::process::id(),
        "reason": reason,
    });
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("write-refusals.jsonl"))
    {
        let _ = writeln!(f, "{line}");
    }
}

/// How this process signs as `router_agent`, resolved once for every kind of
/// write.
///
/// `Ok(Some(..))` — sign. `Ok(None)` — no key exists at all and enforcement
/// is off, which is the legitimate bootstrap case (a fresh home, a test, a
/// workflow channel with no agent behind it); unsigned is correct there.
/// `Err` — `MUR_CHANNEL_REQUIRE_SIG` is set and the key is absent or
/// unreadable (#2142: absent used to slip through as `Ok(None)`).
///
/// One derivation on purpose: an append and a state transition on the same
/// channel must not disagree about whether that channel is protected.
/// Where the writer key came from — which decides whether this process may
/// advance the chain-head anchor after it appends.
///
/// `channel-heads/` is outside every agent's sandbox grant on purpose: an
/// agent may append (its grant covers `channels/`) but must never move the
/// trusted head. A key that was *read from disk* proves this process is
/// outside that sandbox (`keys/` is denied inside it); a key that arrived by
/// handoff proves the opposite. So the source, not the caller, is what
/// selects between `append_and_anchor` and plain `append_signed`.
///
/// INVARIANT (implicit, so it is pinned by a test): `Disk` is chosen ⟺ the
/// caller is outside the sandbox ⟺ `channel-heads/` is writable. This holds
/// only while `keys/` stays unreadable inside the sandbox. Moving the signing
/// key anywhere the sandbox can read (env var, agent home, a readable carve-in)
/// breaks the implication silently — see
/// `mur_agent_runtime::sandbox::policy::tests::fs_grants::`
/// `key_source_invariant_keys_unreadable_iff_channel_heads_unwritable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeySource {
    /// Loaded from `<home>/agents/<name>/identity.*` — trusted, anchors.
    Disk,
    /// Received over stdin from the sealing parent — sandboxed, never anchors.
    Handoff,
}

/// A resolved writer identity plus the provenance that decides anchoring.
pub(crate) struct WriterKey {
    pub(crate) id: mur_common::identity::AgentIdentity,
    pub(crate) kv: u32,
    pub(crate) source: KeySource,
}

pub(crate) fn writer_key(home: &Path, router_agent: &str) -> anyhow::Result<Option<WriterKey>> {
    writer_key_with(
        home,
        router_agent,
        crate::channel_verify::require_sig_from_env(),
    )
}

/// [`writer_key`] with the enforcement policy passed in rather than read from
/// the environment, so both sides of it are testable without mutating
/// process-global env (which `cargo test`'s parallel threads would race on).
fn writer_key_with(
    home: &Path,
    router_agent: &str,
    require_sig: bool,
) -> anyhow::Result<Option<WriterKey>> {
    resolve_writer_key(
        home,
        router_agent,
        Refuse {
            require_sig,
            awaits_answer: false,
        },
    )
}

/// When an unsigned write must be refused rather than made.
#[derive(Debug, Clone, Copy)]
struct Refuse {
    /// `MUR_CHANNEL_REQUIRE_SIG`: refuse whenever the key is absent or
    /// unreadable — the verifying reader would drop the event anyway.
    require_sig: bool,
    /// The event is a `HitlRequest` someone must answer (#2379). Unsigned,
    /// `hitl::authority` never treats it as the router's question, so no
    /// answer can ever release it: refuse an UNREADABLE key regardless of
    /// `require_sig`. An ABSENT key is left to `require_sig` alone — that
    /// is the keyless bootstrap home, which has no router to answer as.
    awaits_answer: bool,
}

fn resolve_writer_key(
    home: &Path,
    router_agent: &str,
    refuse: Refuse,
) -> anyhow::Result<Option<WriterKey>> {
    let Refuse {
        require_sig,
        awaits_answer,
    } = refuse;
    // A process sealed inside an agent's sandbox cannot read `keys/` — that
    // subtree is denied on purpose. Its parent loaded the key before sealing
    // and handed it over stdin, so prefer that when it is for THIS writer.
    // Checked before the disk read because the disk read is the thing that
    // cannot work here, not a faster path we are skipping.
    if let Some((id, kv)) = handoff_for(router_agent) {
        return Ok(Some(WriterKey {
            id: id.clone(),
            kv,
            source: KeySource::Handoff,
        }));
    }
    let agent_home = home.join("agents").join(router_agent);
    match mur_common::identity::AgentIdentity::load(&agent_home) {
        Ok(id) => Ok(Some(WriterKey {
            kv: read_key_version(&agent_home),
            id,
            source: KeySource::Disk,
        })),
        // No key at all. With enforcement off this is the normal bootstrap
        // state (a fresh home, a test fixture) and unsigned is the intended
        // outcome. With it ON, the reader drops an unsigned event, so writing
        // one only moves the failure to verification — where the event just
        // vanishes and nothing says why (#2142). Refuse here instead, exactly
        // as the unreadable case below does.
        Err(mur_common::identity::IdentityError::NotFound) => {
            if require_sig {
                let reason = format!(
                    "refusing to write an unsigned event as '{router_agent}': the writer key \
                     does not exist (expected under {}), and MUR_CHANNEL_REQUIRE_SIG is set",
                    agent_home.display()
                );
                record_refusal(home, router_agent, &reason);
                anyhow::bail!(reason);
            }
            Ok(None)
        }
        // The key is THERE and we may not read it — a sandbox deny (a spawned
        // `mur` cannot read a sibling's signing key since #975). Falling back
        // to unsigned here is a silent security downgrade: the event was meant
        // to be signed, and with `require_sig` off the reader accepts it, so
        // the whole v3d signing guarantee lapses with nothing to show for it.
        // The mirror of the read-side fix in `channel_verify::verify_event`.
        Err(e) => {
            if awaits_answer {
                // Typed, so a caller that knows the remedy (the write-grant
                // gate: "grant it from your own terminal") can say it.
                let reason = format!(
                    "cannot sign an approval request as '{router_agent}': the writer key is \
                     unreadable ({e}), and an unsigned request could never be approved"
                );
                record_refusal(home, router_agent, &reason);
                return Err(UnanswerableRequest(reason).into());
            }
            if require_sig {
                // The reader would reject an unsigned event anyway; fail here,
                // where the cause is still legible, instead of at verification.
                let reason = format!(
                    "refusing to write an unsigned event as '{router_agent}': the writer key \
                     is unreadable ({e}), and MUR_CHANNEL_REQUIRE_SIG is set"
                );
                // Leave a trace before unwinding: this Err is discarded by
                // several call sites, and a sandboxed child's stderr is not
                // reliably captured anywhere.
                record_refusal(home, router_agent, &reason);
                anyhow::bail!(reason);
            }
            tracing::warn!(
                router_agent,
                error = %e,
                "writer signing key is present but unreadable — writing UNSIGNED. \
                 Signature verification is not protecting this channel while that holds."
            );
            Ok(None)
        }
    }
}

/// Move `channel_id` to `new_state`, SIGNED by `router_agent` when possible.
///
/// The signed counterpart of `ChannelService::transition`. A run's start and
/// end are the two events every surface reads to decide whether work is in
/// flight; leaving them unattributable while the messages between them are
/// signed is the wrong way round.
pub fn transition_as_writer(
    svc: &ChannelService,
    home: &Path,
    channel_id: &str,
    router_agent: &str,
    new_state: mur_common::channel::ChannelState,
    actor: ChannelActor,
    run_id: Option<&str>,
) -> anyhow::Result<ChannelEvent> {
    match writer_key(home, router_agent)? {
        // Same provenance rule as `append_with_key`: only a key read from
        // disk proves this process is outside the sandbox, so only it anchors.
        Some(WriterKey {
            id,
            kv,
            source: KeySource::Disk,
        }) => svc.transition_and_anchor(
            channel_id,
            new_state,
            actor,
            run_id,
            &id,
            kv,
            &home.join("agents").join(router_agent),
        ),
        Some(WriterKey { id, kv, .. }) => {
            svc.transition_signed(channel_id, new_state, actor, run_id, Some((&id, kv)))
        }
        None => svc.transition_signed(channel_id, new_state, actor, run_id, None),
    }
}

/// Append `actor`/`kind`/`payload` to `channel_id`, SIGNED by `router_agent`'s
/// identity when it is available, else unsigned (migration-safe).
///
/// `home` is the `~/.mur` root; the router identity is loaded from
/// `<home>/agents/<router_agent>/identity.{key,pub}` and its key version from
/// that agent's `profile.yaml`.
#[allow(clippy::too_many_arguments)]
pub fn append_as_writer(
    svc: &ChannelService,
    home: &Path,
    channel_id: &str,
    router_agent: &str,
    actor: ChannelActor,
    kind: EventKind,
    payload: serde_json::Value,
    idem: Option<String>,
) -> anyhow::Result<ChannelEvent> {
    append_as_writer_with(
        svc,
        home,
        channel_id,
        router_agent,
        actor,
        kind,
        payload,
        idem,
        crate::channel_verify::require_sig_from_env(),
    )
}

/// [`append_as_writer`] with the enforcement policy passed in, for the same
/// reason as [`writer_key_with`]: a test of the keyless path must not change
/// meaning when `MUR_CHANNEL_REQUIRE_SIG` is set in the test process (#2145).
#[allow(clippy::too_many_arguments)]
fn append_as_writer_with(
    svc: &ChannelService,
    home: &Path,
    channel_id: &str,
    router_agent: &str,
    actor: ChannelActor,
    kind: EventKind,
    payload: serde_json::Value,
    idem: Option<String>,
    require_sig: bool,
) -> anyhow::Result<ChannelEvent> {
    match writer_key_with(home, router_agent, require_sig)? {
        Some(key) => {
            let key_home = home.join("agents").join(router_agent);
            append_with_key(svc, channel_id, key, &key_home, actor, kind, payload, idem)
        }
        None => svc.append(channel_id, actor, kind, payload, idem),
    }
}

/// A `HitlRequest` that would have to be written unsigned, refused (#2379).
///
/// Its own type so the caller can tell "this process cannot ask" apart from
/// any other write failure and point at the surface that can.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct UnanswerableRequest(pub String);

/// Append a `HitlRequest`, SIGNED by `router_agent`.
///
/// [`append_as_writer`] with one extra refusal: when `awaits_answer` (a
/// human must release the gate) and the router key exists but this process
/// cannot read it, refuse instead of writing unsigned. An unsigned request
/// is one `hitl::authority` will never answer, so writing it only parks a
/// card nobody can approve (#2379). Without `awaits_answer` (`--yes`, a
/// pre-approved tier) the request is an audit record and keeps the old
/// warn-and-write behaviour.
pub fn append_hitl_request(
    svc: &ChannelService,
    home: &Path,
    channel_id: &str,
    router_agent: &str,
    payload: serde_json::Value,
    awaits_answer: bool,
) -> anyhow::Result<ChannelEvent> {
    let refuse = Refuse {
        require_sig: crate::channel_verify::require_sig_from_env(),
        awaits_answer,
    };
    append_request_with(svc, home, channel_id, router_agent, payload, refuse)
}

fn append_request_with(
    svc: &ChannelService,
    home: &Path,
    channel_id: &str,
    router_agent: &str,
    payload: serde_json::Value,
    refuse: Refuse,
) -> anyhow::Result<ChannelEvent> {
    let (actor, kind) = (ChannelActor::System, EventKind::HitlRequest);
    match resolve_writer_key(home, router_agent, refuse)? {
        Some(key) => {
            let key_home = home.join("agents").join(router_agent);
            append_with_key(svc, channel_id, key, &key_home, actor, kind, payload, None)
        }
        None => svc.append(channel_id, actor, kind, payload, None),
    }
}

/// Append signed with `key`, anchoring the chain head when — and only when —
/// the key's provenance proves this process is outside the agent sandbox.
///
/// The anchor is the one write a sandboxed agent must not be able to make,
/// so the decision lives on the key, where it is a property of the process,
/// and not at the call site, where it would be a property of whoever
/// remembered to pick the right method.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_with_key(
    svc: &ChannelService,
    channel_id: &str,
    key: WriterKey,
    key_home: &Path,
    actor: ChannelActor,
    kind: EventKind,
    payload: serde_json::Value,
    idem: Option<String>,
) -> anyhow::Result<ChannelEvent> {
    let WriterKey { id, kv, source } = key;
    match source {
        KeySource::Disk => {
            svc.append_and_anchor(channel_id, &id, kv, key_home, actor, kind, payload, idem)
        }
        KeySource::Handoff => svc.append_signed(channel_id, &id, kv, actor, kind, payload, idem),
    }
}

#[cfg(test)]
#[path = "channel_writer_missing_key_tests.rs"]
mod missing_key_tests;

#[cfg(test)]
#[path = "channel_writer_request_tests.rs"]
mod request_tests;

#[cfg(test)]
#[path = "channel_writer_tests.rs"]
pub(crate) mod tests;
