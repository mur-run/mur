//! Append channel events signed by the channel's router/owner agent (v3d).
//!
//! The channel's WRITER (the router/owner agent, e.g. the `"mur"` concierge)
//! signs the events it appends so a downstream reader can verify authority
//! before trusting an event (notably the HITL gate verifying a `HitlResponse`).
//!
//! Migration-safe: if the router's identity is unavailable we fall back to an
//! unsigned `append`, so existing channels (and tests that never created an
//! identity) keep working. Enforcement is opt-in via `MUR_CHANNEL_REQUIRE_SIG`
//! on the verification side.
use std::path::Path;

use mur_channel::ChannelService;
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};

/// The channel's router/owner agent — the trusted WRITER that signs the events
/// appended to workflow/HITL channels (v3d). The concierge `"mur"` owns these
/// channels; its on-disk identity (`<home>/agents/mur/`) signs `HitlRequest`/
/// `HitlResponse`/workflow events, and the HITL gate verifies an incoming
/// `HitlResponse` against this agent's pubkey before releasing. Internal
/// directory slug, so lowercase (matches the on-disk `name`).
pub const ROUTER_AGENT: &str = "mur";

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
fn read_key_version(agent_home: &Path) -> u32 {
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
/// `Ok(Some(..))` — sign. `Ok(None)` — no key exists at all, which is the
/// legitimate bootstrap case (a fresh home, a test, a workflow channel with
/// no agent behind it); unsigned is correct there. `Err` — the key is present
/// and unreadable while `MUR_CHANNEL_REQUIRE_SIG` is set.
///
/// One derivation on purpose: an append and a state transition on the same
/// channel must not disagree about whether that channel is protected.
fn writer_key(
    home: &Path,
    router_agent: &str,
) -> anyhow::Result<Option<(mur_common::identity::AgentIdentity, u32)>> {
    // A process sealed inside an agent's sandbox cannot read `keys/` — that
    // subtree is denied on purpose. Its parent loaded the key before sealing
    // and handed it over stdin, so prefer that when it is for THIS writer.
    // Checked before the disk read because the disk read is the thing that
    // cannot work here, not a faster path we are skipping.
    if let Some((id, kv)) = handoff_for(router_agent) {
        return Ok(Some((id.clone(), kv)));
    }
    let agent_home = home.join("agents").join(router_agent);
    match mur_common::identity::AgentIdentity::load(&agent_home) {
        Ok(id) => Ok(Some((id, read_key_version(&agent_home)))),
        Err(mur_common::identity::IdentityError::NotFound) => Ok(None),
        // The key is THERE and we may not read it — a sandbox deny (a spawned
        // `mur` cannot read a sibling's signing key since #975). Falling back
        // to unsigned here is a silent security downgrade: the event was meant
        // to be signed, and with `require_sig` off the reader accepts it, so
        // the whole v3d signing guarantee lapses with nothing to show for it.
        // The mirror of the read-side fix in `channel_verify::verify_event`.
        Err(e) => {
            if crate::channel_verify::require_sig_from_env() {
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
    let key = writer_key(home, router_agent)?;
    svc.transition_signed(
        channel_id,
        new_state,
        actor,
        run_id,
        key.as_ref().map(|(id, kv)| (id, *kv)),
    )
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
    match writer_key(home, router_agent)? {
        Some((id, kv)) => svc.append_signed(channel_id, &id, kv, actor, kind, payload, idem),
        None => svc.append(channel_id, actor, kind, payload, idem),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::identity::AgentIdentity;
    use tempfile::TempDir;

    /// `REFUSALS` is process-wide, and `cargo test` runs these in parallel in
    /// one process — so any test that asserts on a COUNT must hold this lock.
    /// Without it, an unrelated test recording a refusal lands inside another
    /// test's window and the assertion fails for a reason that has nothing to
    /// do with the behaviour under test.
    static COUNTER: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// An UNREADABLE writer key must not silently produce an unsigned event.
    ///
    /// This is the write-side mirror of `channel_verify::verify_event`: there,
    /// an unreadable key must not pass as an absent signature; here, it must
    /// not pass as "no key, so unsigned is fine". A sandboxed `mur` cannot read
    /// a sibling's signing key (#975), so before this every channel event such
    /// a process wrote was unsigned — and with `require_sig` off the reader
    /// accepted it, so the signing guarantee lapsed with no signal anywhere.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_writer_key_does_not_silently_write_unsigned() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let agent_home = home.join("agents").join("mur");
        std::fs::create_dir_all(&agent_home).unwrap();
        AgentIdentity::generate().save(&agent_home).unwrap();
        // The private half lives under `keys/<name>/` for an agent home
        // (#850 option (c)), so chmod the file where it actually is.
        let key = mur_common::identity::private_key_dir(&agent_home).join("identity.key");
        let svc = ChannelService::open(home).unwrap();
        let ch = svc.create_for_workflow("g").unwrap();

        // Precondition: with the key readable, the event IS signed.
        let signed = append_as_writer(
            &svc,
            home,
            &ch.id,
            "mur",
            ChannelActor::System,
            EventKind::Message,
            serde_json::json!({"text": "before"}),
            None,
        )
        .unwrap();
        assert!(signed.sig.is_some(), "precondition: signing works");

        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o000)).unwrap();
        // MUR_CHANNEL_REQUIRE_SIG is not set in the test env, so this takes the
        // warn-and-write path — the event is unsigned, but LOUDLY so. What is
        // asserted here is the discrimination itself: the loader must report
        // this as Denied, not NotFound, which is what makes the warning
        // possible at all.
        let err = mur_common::identity::AgentIdentity::load(&agent_home).unwrap_err();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();

        assert!(
            matches!(err, mur_common::identity::IdentityError::Denied(_)),
            "an unreadable writer key must be distinguishable from an absent \
             one, or the unsigned fallback stays silent: {err:?}"
        );
    }

    /// A refusal must leave something behind that outlives the process.
    ///
    /// `writer_key` returning `Err` is not enough on its own: `dag.rs` drops
    /// that error at three call sites (`let _ = append_as_writer(..)`), and a
    /// sandboxed child's stderr is not reliably captured. Without this
    /// sidecar, a refused write and a write that never happened look
    /// identical after the fact.
    #[test]
    fn a_refusal_leaves_a_readable_trace() {
        // Bumps the process-global counter twice, so it must take the same
        // lock as the counting tests or it lands inside their window.
        let _guard = COUNTER.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let sidecar = home.join("channels").join("write-refusals.jsonl");
        assert!(!sidecar.exists(), "precondition: no refusals yet");

        record_refusal(home, "mur", "writer key unreadable (test)");

        let body = std::fs::read_to_string(&sidecar).expect("refusal sidecar must exist");
        let rec: serde_json::Value = serde_json::from_str(body.trim()).expect("one JSON line");
        assert_eq!(rec["router_agent"], "mur");
        assert!(
            rec["reason"].as_str().unwrap().contains("unreadable"),
            "the reason must survive, not just the fact of a refusal: {rec}"
        );
        assert!(
            rec["ts"].is_string(),
            "a refusal without a time is not evidence"
        );

        // Append-only: a second refusal must not overwrite the first.
        record_refusal(home, "mur", "second");
        let body = std::fs::read_to_string(&sidecar).unwrap();
        assert_eq!(body.lines().count(), 2, "refusals accumulate, not replace");
    }

    /// The counter must survive the sidecar failing.
    ///
    /// A closing summary that only counts refusals it managed to WRITE would
    /// go quiet in the worst case — an unwritable home, where every refusal
    /// is lost and the summary is the last line of defence. Counting happens
    /// before the write is attempted, so this holds.
    #[test]
    fn a_refusal_is_counted_even_when_the_sidecar_cannot_be_written() {
        let _guard = COUNTER.lock().unwrap_or_else(|e| e.into_inner());
        let mark = refusal_count();
        // A path under a regular FILE: create_dir_all cannot succeed here,
        // so record_refusal takes its early return without writing anything.
        let tmp = TempDir::new().unwrap();
        let not_a_dir = tmp.path().join("occupied");
        std::fs::write(&not_a_dir, b"x").unwrap();

        record_refusal(&not_a_dir, "mur", "sidecar unwritable");

        assert_eq!(
            refusals_since(mark),
            1,
            "a refusal that could not be written down is the one most worth \
             counting — otherwise the run reports a clean history it does not have"
        );
    }

    /// `refusals_since` reports a window, not a total.
    #[test]
    fn refusals_since_counts_only_what_followed_the_mark() {
        let _guard = COUNTER.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = TempDir::new().unwrap();
        record_refusal(tmp.path(), "mur", "before the mark");
        let mark = refusal_count();
        assert_eq!(refusals_since(mark), 0, "a fresh mark starts at zero");

        record_refusal(tmp.path(), "mur", "after the mark");
        assert_eq!(refusals_since(mark), 1);

        // A stale/bogus mark must not panic in a reporting path.
        assert_eq!(refusals_since(usize::MAX), 0);
    }

    #[test]
    fn unsigned_when_identity_absent() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("g").unwrap();
        let ev = append_as_writer(
            &svc,
            tmp.path(),
            &ch.id,
            "mur",
            ChannelActor::System,
            EventKind::Note,
            serde_json::json!({ "text": "hi" }),
            None,
        )
        .unwrap();
        assert!(ev.sig.is_none(), "no identity → unsigned (migration-safe)");
    }

    #[test]
    fn signed_when_identity_present_and_verifies() {
        let tmp = TempDir::new().unwrap();
        let svc = ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_workflow("g").unwrap();
        // Plant the router identity under <home>/agents/mur/.
        let agent_home = tmp.path().join("agents").join("mur");
        std::fs::create_dir_all(&agent_home).unwrap();
        let id = AgentIdentity::generate();
        id.save(&agent_home).unwrap();

        let ev = append_as_writer(
            &svc,
            tmp.path(),
            &ch.id,
            "mur",
            ChannelActor::System,
            EventKind::HitlResponse,
            serde_json::json!({ "allow": true }),
            None,
        )
        .unwrap();
        assert!(ev.sig.is_some(), "identity present → signed");
        let pubkey = id.verifying_key_bytes();
        assert!(
            mur_channel::sign::verify_one(&ch.id, &ev, &pubkey, true),
            "signed event must verify against the router pubkey even when require_sig"
        );
    }
}
