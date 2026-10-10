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
    // `respond_hitl` only answers a request the router signed.
    let router = crate::channel_writer::plant_writer_identity(tmp.path());
    let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_agent("mur").unwrap();
    // A pending HitlRequest the phone will respond to.
    svc.append_signed(
        &ch.id,
        &router,
        0,
        ChannelActor::System,
        EventKind::HitlRequest,
        serde_json::json!({
            "hitl_id": "h1", "action_hash": "AH", "tier": "destructive",
            "tool_name": "bash", "tool_input": {}, "step_or_call_id": "s0",
            "agent_id": "mur", "timeout_ms": 300000u64, "summary": "rm -rf x",
            "issued_at": chrono::Utc::now()
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
    let router = crate::channel_writer::plant_writer_identity(tmp.path());
    let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_agent("mur").unwrap();
    for (hid, ah) in [("h1", "AH1"), ("h2", "AH2")] {
        svc.append_signed(
            &ch.id,
            &router,
            0,
            ChannelActor::System,
            EventKind::HitlRequest,
            serde_json::json!({
                "hitl_id": hid, "action_hash": ah, "tier": "destructive",
                "tool_name": "bash", "tool_input": {}, "step_or_call_id": "s0",
                "agent_id": "mur", "timeout_ms": 300000u64, "summary": "x",
            "issued_at": chrono::Utc::now()
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
    let router = crate::channel_writer::plant_writer_identity(tmp.path());
    let svc = mur_channel::ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_agent("mur").unwrap();
    svc.append_signed(
        &ch.id,
        &router,
        0,
        ChannelActor::System,
        EventKind::HitlRequest,
        serde_json::json!({
            "hitl_id": "h1", "action_hash": "AH", "tier": "destructive",
            "tool_name": "bash", "tool_input": {}, "step_or_call_id": "s0",
            "agent_id": "mur", "timeout_ms": 300000u64, "summary": "x",
            "issued_at": chrono::Utc::now()
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
    // The window file holds the plaintext token (0600) — the daemon recomputes
    // the HMAC proof against it; the token is never TRANSMITTED on the wire.
    let raw = std::fs::read_to_string(pair_window_path(home)).unwrap();
    assert!(
        raw.contains(&token),
        "window file stores the token for HMAC recompute"
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
        serde_json::from_str(&std::fs::read_to_string(pair_window_path(home)).unwrap()).unwrap();
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
        serde_json::from_str(&std::fs::read_to_string(pair_window_path(home)).unwrap()).unwrap();
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

#[test]
fn verify_hello_proof_accepts_correct_token_and_is_single_use() {
    use mur_common::mobile::{
        PAIR_ROLE_DAEMON_TO_PHONE, PAIR_ROLE_PHONE_TO_DAEMON, pair_proof, pair_transcript,
    };
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let (wid, token) = mint_pair_window(home, "mur").unwrap();
    let (proto, agent, did, pubkey) = (2u32, "mur", "zDAEMON", "zPHONE");
    let nonce = new_challenge_nonce();
    let nonce_b = nonce.as_bytes();

    // Phone computes the proof from the token (never sent).
    let good = pair_proof(
        token.as_bytes(),
        &pair_transcript(
            PAIR_ROLE_PHONE_TO_DAEMON,
            proto,
            agent,
            &wid,
            did,
            pubkey,
            nonce_b,
        ),
    );
    let confirm = verify_hello_proof(home, &wid, proto, agent, did, pubkey, nonce_b, &good)
        .expect("correct proof accepted");
    // Confirm MAC matches the daemon→phone transcript (phone will verify this).
    let expect_confirm = pair_proof(
        token.as_bytes(),
        &pair_transcript(
            PAIR_ROLE_DAEMON_TO_PHONE,
            proto,
            agent,
            &wid,
            did,
            pubkey,
            nonce_b,
        ),
    );
    assert_eq!(confirm, expect_confirm.to_vec());
    // Single-use: window burned, a second proof fails.
    assert!(
        verify_hello_proof(home, &wid, proto, agent, did, pubkey, nonce_b, &good).is_none(),
        "window is single-use"
    );
}

#[test]
fn verify_hello_proof_rejects_wrong_token_and_bad_nonce() {
    use mur_common::mobile::{PAIR_ROLE_PHONE_TO_DAEMON, pair_proof, pair_transcript};
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    let (wid, _token) = mint_pair_window(home, "mur").unwrap();
    let (proto, agent, did, pubkey) = (2u32, "mur", "zDAEMON", "zPHONE");
    let nonce = new_challenge_nonce();
    // Proof computed with the WRONG token → rejected, window survives (until cap).
    let bad = pair_proof(
        b"not-the-token",
        &pair_transcript(
            PAIR_ROLE_PHONE_TO_DAEMON,
            proto,
            agent,
            &wid,
            did,
            pubkey,
            nonce.as_bytes(),
        ),
    );
    assert!(
        verify_hello_proof(
            home,
            &wid,
            proto,
            agent,
            did,
            pubkey,
            nonce.as_bytes(),
            &bad
        )
        .is_none()
    );
    assert!(
        lookup_pair_window(home, &wid).is_some(),
        "one bad attempt doesn't burn"
    );
    // Unknown wid → None.
    assert!(
        verify_hello_proof(
            home,
            "nope",
            proto,
            agent,
            did,
            pubkey,
            nonce.as_bytes(),
            &bad
        )
        .is_none()
    );
}
