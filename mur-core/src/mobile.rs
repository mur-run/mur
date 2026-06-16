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
// there is no persistent pairing secret. `mur agent pair` / the Hub mints one
// (writing only the token HASH + a TTL to a 0600 file); the daemon claims it
// once at Hello time and burns it. The plaintext token travels out-of-band in
// the QR/URI only — never to disk, never to mDNS. Already-paired devices never
// need a window again (they authenticate by their key in `paired.json`).

fn pair_window_path(home: &Path) -> PathBuf {
    home.join(PAIR_WINDOW_FILE)
}

/// Hex SHA-256 of `s`. The window file stores the token's hash, so even a 0600
/// read does not yield a usable enrollment secret.
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
    token_hash: String,
    agent: String,
    /// Unix-epoch seconds after which the window is dead.
    expires_at: u64,
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Open a fresh pairing window: mint a 122-bit token, persist only its hash + a
/// TTL (mode 0600), and return `(window_id, token)`. Replaces any prior window
/// (one active window at a time). The plaintext token is returned for OOB
/// delivery (QR/URI) and is never written to disk.
pub fn mint_pair_window(home: &Path, agent: &str) -> Result<(String, String)> {
    let token = uuid::Uuid::new_v4().to_string();
    let window_id = uuid::Uuid::new_v4().to_string();
    let wf = PairWindowFile {
        window_id: window_id.clone(),
        token_hash: sha256_hex(&token),
        agent: agent.to_string(),
        expires_at: now_unix() + pair_window_ttl_secs(),
    };
    let path = pair_window_path(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&wf)?)
        .with_context(|| format!("write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok((window_id, token))
}

/// Atomically CLAIM the open pairing window with `token`: succeeds at most once.
/// On a hash match against a non-expired window it deletes the window file
/// (single-use burn) and returns true; otherwise false (and sweeps an expired
/// window). The caller MUST hold the cross-transport enrollment lock so
/// read+burn is atomic, then record the device via [`add_paired_device`].
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
    if !ct_eq(wf.token_hash.as_bytes(), sha256_hex(token).as_bytes()) {
        return false;
    }
    let _ = std::fs::remove_file(&path); // burn: single-use
    true
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

/// Build the pairing URI encoded into the QR. The phone parses host/port/token
/// and calls `connect_lan`. `window_id` (`wid`) scopes the token to one window;
/// older apps that ignore `wid` still work. Token (uuid) + slug are URL-safe.
pub fn pairing_uri(host: &str, port: u16, window_id: &str, token: &str, agent: &str) -> String {
    format!("mur-pair://{host}:{port}/?wid={window_id}&token={token}&agent={agent}")
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
        // Echo the pending request's action_hash so the gate's hash check passes.
        let request: mur_common::hitl::HitlRequest = svc
            .load_events(channel_id)?
            .iter()
            .rev()
            .filter(|e| e.kind == EventKind::HitlRequest)
            // Match the request by `hitl_id` INSIDE find_map: a channel can hold
            // several stacked HitlRequest events (one serviced, one pending; or two
            // concurrent v3b delegations), and `.rev()` visits the newest first. A
            // trailing `.filter()` on the collapsed Option would only test that one
            // newest request and drop a valid approval for any older gate.
            .find_map(|e| {
                serde_json::from_value::<mur_common::hitl::HitlRequest>(e.payload.clone())
                    .ok()
                    .filter(|r| r.hitl_id == hitl_id)
            })
            .ok_or_else(|| anyhow::anyhow!("no pending HitlRequest {hitl_id} in {channel_id}"))?;
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
mod tests {
    use super::*;

    #[test]
    fn channel_query_list_and_events() {
        let tmp = tempfile::TempDir::new().unwrap();
        persist_mobile_exchange(tmp.path(), "mur", "hi", "hello");
        // list → one summary with the agent + a turn count.
        let list = channel_query(tmp.path(), "list", None, None).unwrap();
        let arr = list.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["agents"][0], "mur");
        assert!(arr[0]["turns"].as_u64().unwrap() >= 2);
        let cid = arr[0]["id"].as_str().unwrap().to_string();
        // events → the two messages; since_seq filters.
        let evs = channel_query(tmp.path(), "events", Some(cid.clone()), None).unwrap();
        assert_eq!(evs.as_array().unwrap().len(), 2);
        let evs1 = channel_query(tmp.path(), "events", Some(cid), Some(1)).unwrap();
        assert_eq!(evs1.as_array().unwrap().len(), 1);
    }

    #[test]
    fn persist_mobile_exchange_writes_both_turns_to_one_channel() {
        let tmp = tempfile::TempDir::new().unwrap();
        persist_mobile_exchange(
            tmp.path(),
            "mur",
            "what's my schedule?",
            "you have 2 meetings",
        );
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        let id = svc
            .latest_for_agent("mur")
            .unwrap()
            .expect("channel created");
        let evs = svc.load_events(&id).unwrap();
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].payload["text"], "what's my schedule?");
        assert_eq!(evs[1].payload["text"], "you have 2 meetings");
        // Second exchange appends to the SAME channel (shared, like the Hub).
        persist_mobile_exchange(tmp.path(), "mur", "and tomorrow?", "3 meetings");
        assert_eq!(svc.list(10).unwrap().len(), 1);
        assert_eq!(svc.load_events(&id).unwrap().len(), 4);
    }

    #[test]
    fn persist_into_explicit_channel_targets_it() {
        let tmp = tempfile::TempDir::new().unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        let a = svc.create_for_agent("mur").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let _b = svc.create_for_agent("mur").unwrap(); // newer = latest_for_agent
        // An explicit channel_id lands the turn in `a`, NOT the newer `_b`.
        persist_mobile_exchange_into(tmp.path(), "mur", Some(&a.id), "q", "ans");
        assert_eq!(
            svc.load_events(&a.id).unwrap().len(),
            2,
            "explicit id targeted"
        );
        assert_eq!(
            svc.load_events(&_b.id).unwrap().len(),
            0,
            "newer channel untouched"
        );
        // `None` reuses an existing channel (the latest by updated_at — now `a`
        // after the append above), not a 3rd. Don't assert WHICH (timing-fragile).
        persist_mobile_exchange_into(tmp.path(), "mur", None, "q2", "ans2");
        assert_eq!(
            svc.list(10).unwrap().len(),
            2,
            "None reuses a channel, no 3rd"
        );
    }

    #[test]
    fn respond_hitl_writes_a_hitl_response_echoing_the_request() {
        let tmp = tempfile::TempDir::new().unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        // A pending HitlRequest the phone will respond to.
        svc.append(
            &ch.id,
            ChannelActor::System,
            EventKind::HitlRequest,
            serde_json::json!({
                "hitl_id": "h1", "action_hash": "AH", "tier": "destructive",
                "tool_name": "bash", "tool_input": {}, "step_or_call_id": "s0",
                "agent_id": "mur", "timeout_ms": 300000u64, "summary": "rm -rf x"
            }),
            None,
        )
        .unwrap();
        respond_hitl(tmp.path(), &ch.id, "h1", true, "ok from phone");
        let resp = svc
            .load_events(&ch.id)
            .unwrap()
            .into_iter()
            .find(|e| e.kind == EventKind::HitlResponse)
            .expect("HitlResponse written");
        assert_eq!(resp.payload["hitl_id"], "h1");
        assert_eq!(resp.payload["action_hash"], "AH", "echoes the request hash");
        assert_eq!(resp.payload["allow"], true);
        assert_eq!(resp.payload["surface"], "ios");
        // No pending request → no-op (best-effort).
        respond_hitl(tmp.path(), &ch.id, "nope", false, "");
        assert_eq!(
            svc.load_events(&ch.id)
                .unwrap()
                .iter()
                .filter(|e| e.kind == EventKind::HitlResponse)
                .count(),
            1,
            "unknown hitl_id writes nothing"
        );
    }

    #[test]
    fn respond_hitl_resolves_an_older_stacked_gate_not_just_the_newest() {
        // A channel can hold several HitlRequest events; the phone may approve any
        // of them. respond_hitl must echo the action_hash of the TARGETED hitl_id,
        // not whichever request is newest.
        let tmp = tempfile::TempDir::new().unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        for (hid, ah) in [("h1", "AH1"), ("h2", "AH2")] {
            svc.append(
                &ch.id,
                ChannelActor::System,
                EventKind::HitlRequest,
                serde_json::json!({
                    "hitl_id": hid, "action_hash": ah, "tier": "destructive",
                    "tool_name": "bash", "tool_input": {}, "step_or_call_id": "s0",
                    "agent_id": "mur", "timeout_ms": 300000u64, "summary": "x"
                }),
                None,
            )
            .unwrap();
        }
        // Approve the OLDER gate (h1) while h2 is the newest request.
        respond_hitl(tmp.path(), &ch.id, "h1", true, "ok");
        let resp = svc
            .load_events(&ch.id)
            .unwrap()
            .into_iter()
            .find(|e| e.kind == EventKind::HitlResponse)
            .expect("HitlResponse written for the older gate");
        assert_eq!(resp.payload["hitl_id"], "h1");
        assert_eq!(
            resp.payload["action_hash"], "AH1",
            "echoes h1's hash, not h2's"
        );
    }

    #[test]
    fn respond_hitl_from_params_dispatches_a_well_formed_request() {
        let tmp = tempfile::TempDir::new().unwrap();
        let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
        let ch = svc.create_for_agent("mur").unwrap();
        svc.append(
            &ch.id,
            ChannelActor::System,
            EventKind::HitlRequest,
            serde_json::json!({
                "hitl_id": "h1", "action_hash": "AH", "tier": "destructive",
                "tool_name": "bash", "tool_input": {}, "step_or_call_id": "s0",
                "agent_id": "mur", "timeout_ms": 300000u64, "summary": "x"
            }),
            None,
        )
        .unwrap();

        let params = serde_json::json!({
            "channel_id": ch.id, "hitl_id": "h1", "allow": true, "reason": "ok"
        });
        let out = respond_hitl_from_params(tmp.path(), &params);
        assert_eq!(out, Some((ch.id.clone(), "h1".to_string())));
        assert_eq!(
            svc.load_events(&ch.id)
                .unwrap()
                .iter()
                .filter(|e| e.kind == EventKind::HitlResponse)
                .count(),
            1,
            "a well-formed dispatch writes exactly one HitlResponse"
        );

        // Malformed params (missing hitl_id) → None, nothing written.
        let bad = serde_json::json!({ "channel_id": ch.id, "allow": true });
        assert_eq!(respond_hitl_from_params(tmp.path(), &bad), None);
        assert_eq!(
            svc.load_events(&ch.id)
                .unwrap()
                .iter()
                .filter(|e| e.kind == EventKind::HitlResponse)
                .count(),
            1,
            "malformed params write nothing"
        );
    }

    #[test]
    fn pair_window_is_single_use_and_rejects_wrong_token() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        // No window → nothing can be consumed.
        assert!(!try_consume_pair_window(home, "anything"), "no window yet");

        let (_wid, token) = mint_pair_window(home, "mur").unwrap();
        // The plaintext token is NOT on disk (only its hash).
        let raw = std::fs::read_to_string(pair_window_path(home)).unwrap();
        assert!(
            !raw.contains(&token),
            "window file must store only the token hash"
        );

        // Wrong token never consumes the window.
        assert!(
            !try_consume_pair_window(home, "wrong"),
            "wrong token rejected"
        );
        assert!(
            pair_window_path(home).exists(),
            "rejected attempt leaves the window"
        );

        // Correct token consumes exactly once (single-use burn).
        assert!(
            try_consume_pair_window(home, &token),
            "correct token consumes"
        );
        assert!(!pair_window_path(home).exists(), "window burned after use");
        assert!(!try_consume_pair_window(home, &token), "cannot be reused");
    }

    #[test]
    fn pair_window_expires() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let (_wid, token) = mint_pair_window(home, "mur").unwrap();
        // Force expiry by rewriting the file with a past `expires_at`.
        let mut wf: PairWindowFile =
            serde_json::from_str(&std::fs::read_to_string(pair_window_path(home)).unwrap())
                .unwrap();
        wf.expires_at = 1; // 1970
        std::fs::write(pair_window_path(home), serde_json::to_string(&wf).unwrap()).unwrap();
        assert!(
            !try_consume_pair_window(home, &token),
            "expired window rejected"
        );
        assert!(!pair_window_path(home).exists(), "expired window swept");
    }

    #[test]
    fn sweep_removes_only_expired_windows() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        // A live window survives the sweep.
        mint_pair_window(home, "mur").unwrap();
        sweep_expired_pair_window(home);
        assert!(pair_window_path(home).exists(), "live window kept");
        // An expired window is removed by the sweep (so a clock rollback finds
        // nothing to revive).
        let mut wf: PairWindowFile =
            serde_json::from_str(&std::fs::read_to_string(pair_window_path(home)).unwrap())
                .unwrap();
        wf.expires_at = 1;
        std::fs::write(pair_window_path(home), serde_json::to_string(&wf).unwrap()).unwrap();
        sweep_expired_pair_window(home);
        assert!(!pair_window_path(home).exists(), "expired window swept");
    }

    #[test]
    fn list_and_remove_paired_devices_by_fingerprint() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        add_paired_device(home, "zKEY_AAA").unwrap();
        add_paired_device(home, "zKEY_BBB").unwrap();
        let devices = list_paired_devices(home);
        assert_eq!(devices.len(), 2);
        let (pk, fp) = devices[0].clone();
        // Remove by fingerprint prefix.
        let removed = remove_paired_device(home, &fp[..6]).unwrap();
        assert_eq!(removed.as_deref(), Some(pk.as_str()));
        assert!(
            !is_device_paired(home, &pk),
            "removed device no longer paired"
        );
        assert_eq!(list_paired_devices(home).len(), 1);
        // Non-matching fragment removes nothing.
        assert_eq!(remove_paired_device(home, "deadbeef").unwrap(), None);
    }

    #[test]
    fn paired_envelope_ok_requires_a_paired_device_and_intact_signature() {
        use mur_common::identity::AgentIdentity;
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let id = AgentIdentity::generate();
        let env = mur_common::bridge::envelope::sign_payload(b"{}".to_vec(), &id, 1);
        let pk = env.bridge_pubkey_multibase.clone();

        // Valid signature but the device has never paired → rejected. This is the
        // crux of the relay fix: self-consistent signing is not enough.
        assert!(!paired_envelope_ok(home, &env), "unpaired device rejected");

        // After the token handshake records the device → accepted.
        add_paired_device(home, &pk).unwrap();
        assert!(is_device_paired(home, &pk));
        assert!(paired_envelope_ok(home, &env), "paired + signed accepted");

        // Tampered payload (signature no longer matches) → rejected even when paired.
        let mut tampered = env.clone();
        tampered.payload = br#"{"x":1}"#.to_vec();
        assert!(
            !paired_envelope_ok(home, &tampered),
            "paired but broken signature rejected"
        );

        // A different device's key is not paired → rejected.
        let other = AgentIdentity::generate();
        let other_env = mur_common::bridge::envelope::sign_payload(b"{}".to_vec(), &other, 1);
        assert!(
            !paired_envelope_ok(home, &other_env),
            "a different (unpaired) device is rejected"
        );
    }

    #[test]
    fn resume_proof_ok_binds_to_the_issued_nonce_and_paired_key() {
        use mur_common::identity::{AgentIdentity, encode_pubkey};
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let id = AgentIdentity::generate();
        let pk = encode_pubkey(&id.verifying_key());
        add_paired_device(home, &pk).unwrap();

        let nonce = new_challenge_nonce();
        let proof = mur_common::bridge::envelope::sign_payload(nonce.as_bytes().to_vec(), &id, 1);

        // Correct device + signature over the exact issued nonce → accepted.
        assert!(
            resume_proof_ok(home, &pk, &nonce, &proof),
            "valid resume proof"
        );

        // A proof over a DIFFERENT nonce is rejected (replay/another connection).
        let other_nonce = new_challenge_nonce();
        assert!(
            !resume_proof_ok(home, &pk, &other_nonce, &proof),
            "proof must sign the daemon-issued nonce"
        );

        // An unpaired device cannot resume even with a valid signature.
        let stranger = AgentIdentity::generate();
        let spk = encode_pubkey(&stranger.verifying_key());
        let sproof =
            mur_common::bridge::envelope::sign_payload(nonce.as_bytes().to_vec(), &stranger, 1);
        assert!(
            !resume_proof_ok(home, &spk, &nonce, &sproof),
            "unpaired device cannot resume"
        );

        // A proof whose envelope key != the claimed pubkey is rejected.
        assert!(
            !resume_proof_ok(home, &pk, &nonce, &sproof),
            "envelope key must match the claimed pubkey"
        );
    }
}
