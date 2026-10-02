//! Shared helpers for the MUR mobile pairing / LAN transport (P1).
//!
//! Used by both the daemon endpoint (`mur-daemon::mobile_server`) and the
//! `mur agent pair` CLI so the QR a user scans always matches the token, port,
//! and paths the daemon actually serves. The wire protocol itself lives in
//! `mur_common::mobile`. Design:
//! `docs/superpowers/specs/2026-06-05-mur-voice-mobile-app-design.md`.

use anyhow::{Context, Result};
use mur_channel::ChannelService;
use mur_common::bridge::envelope::{SignedEnvelope, verify_envelope_with_pubkey};
use mur_common::channel::{ChannelActor, EventKind};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{IpAddr, UdpSocket};
use std::path::{Path, PathBuf};

/// Default LAN port for the mobile WebSocket endpoint (distinct from the
/// signal server's 9421). Override with `MUR_MOBILE_PORT`.
pub const DEFAULT_MOBILE_PORT: u16 = 9430;
/// Default bind address — `0.0.0.0` so a phone on the LAN can reach it.
/// Override with `MUR_MOBILE_BIND`.
pub const DEFAULT_MOBILE_BIND: &str = "0.0.0.0";
/// Agent the phone talks to when none is named (the concierge).
pub const DEFAULT_MOBILE_AGENT: &str = "mur";

const PAIRED_DEVICES_FILE: &str = "mobile/paired.json";
const PAIR_WINDOW_FILE: &str = "mobile/pair-window.json";

/// Default pairing-window lifetime. Pairing is a same-room, human-present
/// ceremony, so the window only needs to outlive "open app, grant camera +
/// local-network permission, aim, scan" — 120s matches Matter/HomeKit/Signal
/// in-person norms while keeping a screenshotted QR useless ~2 min later.
pub const DEFAULT_PAIR_WINDOW_TTL_SECS: u64 = 120;
/// Hard cap on the configurable window TTL (`MUR_PAIR_WINDOW_TTL`).
const MAX_PAIR_WINDOW_TTL_SECS: u64 = 300;
/// How often the daemon sweeps an unclaimed, expired window off disk. Bounds how
/// long a lapsed window lingers — so a wall-clock rollback can't revive a stale
/// window (there is nothing left to read). Expiry uses the wall clock, so this
/// sweep is the practical bound on that narrow rollback case; a local attacker
/// who can both set the clock back AND already holds the out-of-band token is
/// outside the threat model (the token alone authorizes enrollment).
pub const PAIR_WINDOW_SWEEP_SECS: u64 = 30;

/// Effective pairing-window TTL in seconds, honouring `MUR_PAIR_WINDOW_TTL`
/// (clamped to [`MAX_PAIR_WINDOW_TTL_SECS`]).
pub fn pair_window_ttl_secs() -> u64 {
    std::env::var("MUR_PAIR_WINDOW_TTL")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_PAIR_WINDOW_TTL_SECS)
        .min(MAX_PAIR_WINDOW_TTL_SECS)
}

/// Effective mobile port, honouring the `MUR_MOBILE_PORT` override.
pub fn mobile_port() -> u16 {
    std::env::var("MUR_MOBILE_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MOBILE_PORT)
}

/// Effective bind address, honouring the `MUR_MOBILE_BIND` override.
pub fn mobile_bind() -> String {
    std::env::var("MUR_MOBILE_BIND").unwrap_or_else(|_| DEFAULT_MOBILE_BIND.to_string())
}

/// Path to the paired-device store under `<home>/mobile/paired.json`.
pub fn paired_devices_path(home: &Path) -> PathBuf {
    home.join(PAIRED_DEVICES_FILE)
}

/// Load the set of paired device pubkeys (multibase) from `paired.json`. The
/// store is shared by both transports — a device paired over LAN is recognized
/// over relay and vice versa — so authoritative writes are gated identically.
pub fn load_paired_devices(home: &Path) -> HashSet<String> {
    std::fs::read_to_string(paired_devices_path(home))
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
        .map(|v| v.into_iter().collect())
        .unwrap_or_default()
}

/// Persist the paired-device set as pretty JSON (atomic-enough for this tiny,
/// low-churn file). Creates `<home>/mobile/` if missing.
pub fn persist_paired_devices(home: &Path, devices: &[String]) -> Result<()> {
    let path = paired_devices_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(devices)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Whether `pubkey` is a paired device.
pub fn is_device_paired(home: &Path, pubkey: &str) -> bool {
    load_paired_devices(home).contains(pubkey)
}

/// Record `pubkey` as a paired device (idempotent). Called only after the
/// one-time pair token has been verified, on either transport.
pub fn add_paired_device(home: &Path, pubkey: &str) -> Result<()> {
    let mut set = load_paired_devices(home);
    if set.insert(pubkey.to_string()) {
        let all: Vec<String> = set.into_iter().collect();
        persist_paired_devices(home, &all)?;
        tracing::info!(pubkey = %pubkey, "mobile: paired new device");
    }
    Ok(())
}

/// Authorize a signed envelope from the phone: its declared pubkey must be a
/// PAIRED device AND the Ed25519 signature must verify against that same key.
/// This is the per-frame gate the relay transport applies before any write —
/// pinning the envelope to a device that completed the token handshake, instead
/// of trusting any self-consistent signature.
pub fn paired_envelope_ok(home: &Path, envelope: &SignedEnvelope) -> bool {
    let pubkey = &envelope.bridge_pubkey_multibase;
    is_device_paired(home, pubkey) && verify_envelope_with_pubkey(envelope, pubkey).is_ok()
}

/// A fresh, unguessable challenge nonce for a Resume handshake (122-bit UUID).
/// Minted per connection so a captured `ResumeProof` cannot be replayed.
pub fn new_challenge_nonce() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Verify a `Resume` challenge-response (steady-state reconnect auth). The proof
/// envelope must be signed by `pubkey`, carry EXACTLY the daemon-issued `nonce`
/// as its payload, and `pubkey` must be a paired device. The per-connection
/// nonce defeats replay; pairing membership is file-backed (shared by both
/// transports).
pub fn resume_proof_ok(home: &Path, pubkey: &str, nonce: &str, envelope: &SignedEnvelope) -> bool {
    envelope.bridge_pubkey_multibase == pubkey
        && envelope.payload == nonce.as_bytes()
        && is_device_paired(home, pubkey)
        && verify_envelope_with_pubkey(envelope, pubkey).is_ok()
}

// ── Pairing window (enrollment) ──────────────────────────────────────────────
//
// Enrolling a NEW device requires an on-demand, single-use, short-TTL window —
// there is no persistent pairing secret. `mur agent pair` / the Hub mints one,
// writing the token + a TTL to a 0600 file; the daemon recomputes the proof
// HMAC against that token, then burns the window. The token still travels OOB in
// the QR/URI (same value the daemon holds) and is NEVER transmitted on the wire
// — the phone proves possession via HMAC, see `mur_common::mobile::pair_proof`.
// Already-paired devices never need a window again (they auth by their key).

fn pair_window_path(home: &Path) -> PathBuf {
    home.join(PAIR_WINDOW_FILE)
}

/// Hex SHA-256 of `s`. Used for the short device fingerprint shown by
/// `mur agent devices` (the window stores the plaintext token, not a hash, since
/// the daemon must recompute the HMAC proof against it).
fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    hex::encode(h.finalize())
}

/// Constant-time equality for equal-length byte slices (avoids leaking how many
/// leading bytes of a token hash matched).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PairWindowFile {
    window_id: String,
    /// Plaintext token (0600 file): the daemon recomputes HMAC(token, transcript)
    /// for the proof handshake. The same value is exposed OOB via the QR, so this
    /// is not a weaker posture than hash-at-rest.
    token: String,
    agent: String,
    /// Unix-epoch seconds after which the window is dead.
    expires_at: u64,
    /// Failed proof attempts; the window is burned after `MAX_PAIR_ATTEMPTS`.
    #[serde(default)]
    attempts: u32,
}

/// Max failed proof attempts before a window is burned. At 122-bit entropy
/// online guessing is already hopeless; this is defense-in-depth.
const MAX_PAIR_ATTEMPTS: u32 = 5;

/// A live pairing window resolved by id — the data the daemon needs to run the
/// proof handshake.
pub struct PairWindow {
    pub window_id: String,
    pub token: String,
    pub agent: String,
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Open a fresh pairing window: mint a 122-bit token + window id, persist them +
/// a TTL (mode 0600), and return `(window_id, token)`. Replaces any prior window
/// (one active window per home — concurrent `mur agent pair` for two agents
/// collapses to the last; fail-closed, an enroller for a clobbered wid is simply
/// rejected). The token is returned for OOB delivery (QR/URI) — it is never
/// TRANSMITTED on the wire (the phone proves it via HMAC).
pub fn mint_pair_window(home: &Path, agent: &str) -> Result<(String, String)> {
    let token = uuid::Uuid::new_v4().to_string();
    let window_id = uuid::Uuid::new_v4().to_string();
    let wf = PairWindowFile {
        window_id: window_id.clone(),
        token: token.clone(),
        agent: agent.to_string(),
        expires_at: now_unix() + pair_window_ttl_secs(),
        attempts: 0,
    };
    let path = pair_window_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(&wf)?;
    // Create with 0600 ATOMICALLY (mode applies at creation) so the plaintext
    // token is never world-readable for even an instant — no write-then-chmod
    // TOCTOU. The trailing set_permissions re-tightens any pre-existing file.
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        f.write_all(body.as_bytes())
            .with_context(|| format!("write {}", path.display()))?;
    }
    #[cfg(not(unix))]
    std::fs::write(&path, &body).with_context(|| format!("write {}", path.display()))?;
    Ok((window_id, token))
}

/// LEGACY bearer path only: atomically CLAIM the open window with the plaintext
/// `token` (succeeds at most once, single-use burn). Used solely by the gated
/// legacy `Hello{token}` enrollment; the proto≥2 path uses
/// [`lookup_pair_window`] + an HMAC proof so the token is never transmitted.
pub fn try_consume_pair_window(home: &Path, token: &str) -> bool {
    let path = pair_window_path(home);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return false;
    };
    let Ok(wf) = serde_json::from_str::<PairWindowFile>(&raw) else {
        return false;
    };
    if now_unix() >= wf.expires_at {
        let _ = std::fs::remove_file(&path); // sweep expired
        return false;
    }
    if !ct_eq(wf.token.as_bytes(), token.as_bytes()) {
        return false;
    }
    let _ = std::fs::remove_file(&path); // burn: single-use
    true
}

/// Look up the live (non-expired) pairing window with id `wid`, returning its
/// token + agent so the daemon can recompute the HMAC proof. Sweeps + returns
/// `None` if expired; `None` if absent or the id doesn't match.
pub fn lookup_pair_window(home: &Path, wid: &str) -> Option<PairWindow> {
    let path = pair_window_path(home);
    let raw = std::fs::read_to_string(&path).ok()?;
    let wf: PairWindowFile = serde_json::from_str(&raw).ok()?;
    if now_unix() >= wf.expires_at {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    if wf.window_id != wid {
        return None;
    }
    Some(PairWindow {
        window_id: wf.window_id,
        token: wf.token,
        agent: wf.agent,
    })
}

/// Burn (delete) the pairing window if its id matches `wid` — single-use after a
/// successful proof enrollment.
pub fn burn_pair_window(home: &Path, wid: &str) {
    let path = pair_window_path(home);
    if let Ok(raw) = std::fs::read_to_string(&path)
        && let Ok(wf) = serde_json::from_str::<PairWindowFile>(&raw)
        && wf.window_id == wid
    {
        let _ = std::fs::remove_file(&path);
    }
}

/// Record a failed proof attempt against `wid`; burn the window once it reaches
/// [`MAX_PAIR_ATTEMPTS`] (bounds online guessing).
pub fn record_pair_failure(home: &Path, wid: &str) {
    let path = pair_window_path(home);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(mut wf) = serde_json::from_str::<PairWindowFile>(&raw) else {
        return;
    };
    if wf.window_id != wid {
        return;
    }
    wf.attempts += 1;
    if wf.attempts >= MAX_PAIR_ATTEMPTS {
        let _ = std::fs::remove_file(&path);
    } else if let Ok(s) = serde_json::to_string_pretty(&wf) {
        let _ = std::fs::write(&path, s);
    }
}

/// Verify a `HelloProof` against the live window `wid` (proto≥2 enrollment, the
/// shared crypto for BOTH transports). Recomputes the phone→daemon transcript
/// with the window's token and constant-time-compares. On success it BURNS the
/// window (single-use) and returns the daemon→phone `confirm` MAC for the caller
/// to send in `Paired`; the caller must also record the device under the
/// enrollment lock. On failure it records an attempt (burning after
/// `MAX_PAIR_ATTEMPTS`) and returns `None`. `agent` must be the canonical name.
#[allow(clippy::too_many_arguments)]
pub fn verify_hello_proof(
    home: &Path,
    wid: &str,
    proto: u32,
    agent: &str,
    did: &str,
    phone_pubkey: &str,
    nonce: &[u8],
    proof: &[u8],
) -> Option<Vec<u8>> {
    use mur_common::mobile::{
        PAIR_ROLE_DAEMON_TO_PHONE, PAIR_ROLE_PHONE_TO_DAEMON, ct_verify, pair_proof,
        pair_transcript,
    };
    let win = lookup_pair_window(home, wid)?;
    let expect = pair_proof(
        win.token.as_bytes(),
        &pair_transcript(
            PAIR_ROLE_PHONE_TO_DAEMON,
            proto,
            agent,
            wid,
            did,
            phone_pubkey,
            nonce,
        ),
    );
    if !ct_verify(&expect, proof) {
        record_pair_failure(home, wid);
        return None;
    }
    let confirm = pair_proof(
        win.token.as_bytes(),
        &pair_transcript(
            PAIR_ROLE_DAEMON_TO_PHONE,
            proto,
            agent,
            wid,
            did,
            phone_pubkey,
            nonce,
        ),
    );
    burn_pair_window(home, wid);
    Some(confirm.to_vec())
}

/// The daemon agent's stable Ed25519 identity (multibase) — the `did` bound into
/// the pairing QR + proof transcript for endpoint authentication on TLS-less LAN.
pub fn daemon_id(home: &Path, agent: &str) -> Option<String> {
    let agent_home = home.join("agents").join(agent);
    mur_common::identity::AgentIdentity::load(&agent_home)
        .ok()
        .map(|id| id.pubkey_text())
}

/// Whether legacy bearer-token `Hello` enrollment is allowed (env opt-in,
/// `MUR_ALLOW_LEGACY_PAIRING`, default OFF). Proto≥2 enrollment always uses the
/// HMAC proof; this is only a time-boxed escape hatch for operators mid-upgrade,
/// and never applies to the relay (which always requires the proof).
pub fn allow_legacy_pairing() -> bool {
    std::env::var("MUR_ALLOW_LEGACY_PAIRING")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Delete the pairing-window file if it has expired. Cheap to call on a timer
/// (every [`PAIR_WINDOW_SWEEP_SECS`]) so an unclaimed window does not linger past
/// its TTL — bounding any wall-clock-rollback revival to one sweep interval.
pub fn sweep_expired_pair_window(home: &Path) {
    let path = pair_window_path(home);
    if let Ok(raw) = std::fs::read_to_string(&path)
        && let Ok(wf) = serde_json::from_str::<PairWindowFile>(&raw)
        && now_unix() >= wf.expires_at
    {
        let _ = std::fs::remove_file(&path);
    }
}

/// A short, stable, display fingerprint for a paired device pubkey (first 12 hex
/// of its SHA-256). Used by `mur agent devices` / `unpair`.
pub fn device_fingerprint(pubkey: &str) -> String {
    sha256_hex(pubkey)[..12].to_string()
}

/// All paired devices as `(pubkey, fingerprint)`, sorted by fingerprint.
pub fn list_paired_devices(home: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = load_paired_devices(home)
        .into_iter()
        .map(|pk| {
            let fp = device_fingerprint(&pk);
            (pk, fp)
        })
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/// Remove a paired device matching `frag` (its full pubkey or fingerprint
/// prefix). Returns the removed pubkey, or `None` if nothing matched.
pub fn remove_paired_device(home: &Path, frag: &str) -> Result<Option<String>> {
    let set = load_paired_devices(home);
    let Some(pubkey) = set
        .iter()
        .find(|pk| *pk == frag || device_fingerprint(pk).starts_with(frag))
        .cloned()
    else {
        return Ok(None);
    };
    let remaining: Vec<String> = set.into_iter().filter(|pk| pk != &pubkey).collect();
    persist_paired_devices(home, &remaining)?;
    tracing::info!(fingerprint = %device_fingerprint(&pubkey), "mobile: unpaired device");
    Ok(Some(pubkey))
}

/// Best-effort primary LAN IP of this host. No traffic is sent — we open a UDP
/// socket "connected" to a public address and read which local interface the
/// OS would route through. Returns `None` if there is no usable route.
pub fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    socket.local_addr().ok().map(|addr| addr.ip())
}

/// Build the pairing URI encoded into the QR. `wid` scopes the token to one
/// window; `did` is the daemon agent's Ed25519 identity, which the phone
/// cross-checks during the proof handshake to bind the endpoint on TLS-less LAN.
/// `v=2` marks the proof-handshake protocol. All fields are URL-safe.
pub fn pairing_uri(
    host: &str,
    port: u16,
    window_id: &str,
    token: &str,
    did: &str,
    agent: &str,
) -> String {
    format!("mur-pair://{host}:{port}/?wid={window_id}&token={token}&did={did}&agent={agent}&v=2")
}

/// Max channels returned to the phone (v4 scale is small).
const MOBILE_CHANNEL_LIMIT: usize = 200;

/// Serve a channel pull for the phone. `op` ∈ "list" | "events".
/// "list" → array of `{id,title,state,goal,updated_at,agents,turns}` (newest
/// first, empties hidden). "events" → that channel's events at/after `since_seq`.
/// Ownership filter: single-user, so all local channels are the owner's.
pub fn channel_query(
    home: &std::path::Path,
    op: &str,
    channel_id: Option<String>,
    since_seq: Option<u64>,
) -> anyhow::Result<serde_json::Value> {
    let svc = ChannelService::open(home)?;
    match op {
        "list" => {
            let mut out = Vec::new();
            for row in svc.list(MOBILE_CHANNEL_LIMIT)? {
                let events = svc.load_events(&row.id).unwrap_or_default();
                if events.is_empty() {
                    continue;
                }
                let manifest = svc.store().load_manifest(&row.id).ok();
                let agents: Vec<String> = manifest
                    .as_ref()
                    .map(|m| {
                        m.participants
                            .iter()
                            .filter_map(|p| match &p.actor {
                                ChannelActor::Agent { id } => Some(id.clone()),
                                _ => None,
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let goal = manifest
                    .as_ref()
                    .map(|m| m.goal.statement.clone())
                    .unwrap_or_default();
                out.push(serde_json::json!({
                    "id": row.id,
                    "title": row.title,
                    "state": row.state,
                    "goal": goal,
                    "updated_at": row.updated_at,
                    "agents": agents,
                    "turns": events.len(),
                }));
            }
            Ok(serde_json::Value::Array(out))
        }
        "events" => {
            let id = channel_id.ok_or_else(|| anyhow::anyhow!("events query needs channel_id"))?;
            let evs: Vec<_> = svc
                .load_events(&id)?
                .into_iter()
                .filter(|e| since_seq.is_none_or(|s| e.seq >= s))
                .collect();
            Ok(serde_json::to_value(evs)?)
        }
        other => anyhow::bail!("unknown channel query op `{other}`"),
    }
}

/// Persist one mobile user→agent exchange into the agent's channel (resolved
/// once), so phone conversations are durable and shared with the Hub/CLI.
/// Best-effort: failures are logged, never surfaced to the phone. Mirrors the
/// Hub's `chat::persist_exchange`. The channel is created on the first exchange.
pub fn persist_mobile_exchange(
    home: &std::path::Path,
    agent: &str,
    user_text: &str,
    agent_text: &str,
) {
    persist_mobile_exchange_into(home, agent, None, user_text, agent_text)
}

/// Like [`persist_mobile_exchange`] but lands the turn in an EXPLICIT
/// `channel_id` when given (v4c: the phone "drops into" a specific channel — a
/// Hub/CLI-originated one, or a non-latest one), else the agent's latest/new
/// channel. Chat turns stay unsigned `append_message` (mobile chat is not a
/// gate-authority path; the signed path is HITL respond, see `respond_hitl`).
pub fn persist_mobile_exchange_into(
    home: &std::path::Path,
    agent: &str,
    channel_id: Option<&str>,
    user_text: &str,
    agent_text: &str,
) {
    let res = (|| -> anyhow::Result<()> {
        let svc = ChannelService::open(home)?;
        let id = match channel_id {
            Some(id) => id.to_string(),
            None => match svc.latest_for_agent(agent)? {
                Some(id) => id,
                None => svc.create_for_agent(agent)?.id,
            },
        };
        svc.append_message(
            &id,
            ChannelActor::local_human(),
            EventKind::Message,
            user_text,
            None,
        )?;
        svc.append_message(
            &id,
            ChannelActor::Agent {
                id: agent.to_string(),
            },
            EventKind::Message,
            agent_text,
            None,
        )?;
        Ok(())
    })();
    if let Err(e) = res {
        tracing::warn!("mobile channel persist failed for {agent}: {e:#}");
    }
}

/// Respond to a HITL gate on behalf of a paired phone (v4c). The daemon has
/// already verified the frame came from a paired device, so the channel's WRITER
/// (the router, "mur") records a v3d-signed `HitlResponse` that the waiting v3c
/// gate verifies before releasing — a forged response from a non-router key is
/// rejected. Mirrors `cmd::channel::approve`, with `surface = "ios"`. Best-effort.
pub fn respond_hitl(
    home: &std::path::Path,
    channel_id: &str,
    hitl_id: &str,
    allow: bool,
    reason: &str,
) {
    let res = (|| -> anyhow::Result<()> {
        let svc = ChannelService::open(home)?;
        // Echo the pending request's action_hash so the gate's hash check
        // passes — and only a request the router asked (see
        // `hitl::authority::request_to_answer`), matched by id, not merely
        // the newest one.
        let events = svc.load_events(channel_id)?;
        let request =
            crate::hitl::authority::request_to_answer(home, channel_id, &events, hitl_id)?;
        let resp = mur_common::hitl::HitlResponse {
            hitl_id: request.hitl_id,
            action_hash: request.action_hash,
            allow,
            reason: reason.to_string(),
            surface: "ios".into(),
        };
        crate::channel_writer::append_as_writer(
            &svc,
            home,
            channel_id,
            crate::channel_writer::ROUTER_AGENT,
            ChannelActor::local_human(),
            EventKind::HitlResponse,
            serde_json::to_value(&resp)?,
            None,
        )?;
        Ok(())
    })();
    if let Err(e) = res {
        tracing::warn!("mobile hitl respond failed for {channel_id}: {e:#}");
    }
}

/// Dispatch a signed `channel/hitl_respond` A2A request (v4c). The daemon calls
/// this ONLY after it has verified the envelope's Ed25519 signature, so the
/// approval is authentic. Extracts `{channel_id, hitl_id, allow, reason}` from the
/// request params, writes the v3d-signed `HitlResponse`, and returns
/// `(channel_id, hitl_id)` for the caller's ack — `None` if params are malformed
/// (missing channel_id/hitl_id), in which case nothing is written.
pub fn respond_hitl_from_params(
    home: &std::path::Path,
    params: &serde_json::Value,
) -> Option<(String, String)> {
    let channel_id = params.get("channel_id")?.as_str()?.to_string();
    let hitl_id = params.get("hitl_id")?.as_str()?.to_string();
    let allow = params
        .get("allow")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let reason = params
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    respond_hitl(home, &channel_id, &hitl_id, allow, reason);
    Some((channel_id, hitl_id))
}

#[cfg(test)]
mod tests;
