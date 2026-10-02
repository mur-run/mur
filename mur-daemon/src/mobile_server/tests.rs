use super::*;
use futures_util::{SinkExt, StreamExt};
use mur_common::a2a::{JsonRpcRequest, Message as A2aMessage, MessagePart};
use mur_common::bridge::envelope::{SignedEnvelope, sign_payload};
use mur_common::identity::{AgentIdentity, encode_pubkey};
use std::net::SocketAddr;
use std::time::Duration;
use tempfile::TempDir;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

async fn start_server() -> (SocketAddr, TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    seed_agent(&home); // so daemon_id("mur") resolves for HelloInit
    let (chan_tx, _) = tokio::sync::broadcast::channel(8);
    let state = MobileState {
        mur_home: home.clone(),
        enroll_lock: Arc::new(tokio::sync::Mutex::new(())),
        chan_tx,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, tmp)
}

/// Give the "mur" agent an on-disk identity so `daemon_id` returns a `did`.
fn seed_agent(home: &std::path::Path) {
    AgentIdentity::generate()
        .save(&home.join("agents").join("mur"))
        .unwrap();
}

/// Run the full proto≥2 HMAC proof enrollment for a fresh phone identity;
/// returns the paired socket + the phone identity. Panics on any failure.
async fn enroll_via_proof(addr: SocketAddr, home: &std::path::Path) -> (Ws, AgentIdentity) {
    use mur_common::mobile::{PAIR_ROLE_PHONE_TO_DAEMON, pair_proof, pair_transcript};
    let (wid, token) = mur_core::mobile::mint_pair_window(home, "mur").unwrap();
    let id = AgentIdentity::generate();
    let pubkey = encode_pubkey(&id.verifying_key());
    let mut ws = connect(addr).await;
    send_frame(
        &mut ws,
        &ClientFrame::HelloInit {
            proto: 2,
            agent: "mur".to_string(),
            pubkey: pubkey.clone(),
            wid: wid.clone(),
        },
    )
    .await;
    let (nonce, did) = match recv_server(&mut ws).await {
        ServerFrame::PairChallenge { nonce, did, .. } => (nonce, did),
        other => panic!("expected PairChallenge, got {other:?}"),
    };
    let proof = pair_proof(
        token.as_bytes(),
        &pair_transcript(
            PAIR_ROLE_PHONE_TO_DAEMON,
            2,
            "mur",
            &wid,
            &did,
            &pubkey,
            &nonce,
        ),
    );
    send_frame(
        &mut ws,
        &ClientFrame::HelloProof {
            wid,
            proof: proof.to_vec(),
        },
    )
    .await;
    match recv_server(&mut ws).await {
        ServerFrame::Paired { confirm, .. } => {
            assert!(!confirm.is_empty(), "proof pairing returns a confirm MAC")
        }
        other => panic!("expected Paired, got {other:?}"),
    }
    (ws, id)
}

async fn connect(addr: SocketAddr) -> Ws {
    let url = format!("ws://{addr}{MOBILE_WS_PATH}");
    let (ws, _) = tokio_tungstenite::connect_async(url.as_str())
        .await
        .unwrap();
    ws
}

async fn send_frame(ws: &mut Ws, frame: &ClientFrame) {
    let txt = serde_json::to_string(frame).unwrap();
    ws.send(WsMessage::Text(txt.into())).await.unwrap();
}

async fn recv_server(ws: &mut Ws) -> ServerFrame {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("recv timeout")
            .expect("stream ended")
            .expect("ws error");
        if let WsMessage::Text(t) = msg {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

fn make_envelope(id: &AgentIdentity, text: &str) -> SignedEnvelope {
    let msg = A2aMessage {
        role: "user".to_string(),
        parts: vec![MessagePart::Text {
            text: text.to_string(),
        }],
    };
    let mut params = serde_json::Map::new();
    params.insert(
        "agent".to_string(),
        serde_json::Value::String("mur".to_string()),
    );
    params.insert("message".to_string(), serde_json::to_value(&msg).unwrap());
    let req = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: None,
        method: "message/send".to_string(),
        params: Some(serde_json::Value::Object(params)),
    };
    let payload = serde_json::to_vec(&req).unwrap();
    sign_payload(payload, id, 1)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_legacy_hello_when_disabled() {
    // Legacy bearer Hello for a NEW (unpaired) device is refused by default
    // (MUR_ALLOW_LEGACY_PAIRING off) — new devices must use the proof handshake.
    let (addr, _tmp) = start_server().await;
    let mut ws = connect(addr).await;
    send_frame(
        &mut ws,
        &ClientFrame::Hello {
            pubkey: "zBogus".to_string(),
            token: "anything".to_string(),
            agent: "mur".to_string(),
        },
    )
    .await;
    assert!(matches!(
        recv_server(&mut ws).await,
        ServerFrame::Rejected { .. }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enrolls_via_proof_handshake() {
    // Full proto≥2 enrollment: HelloInit → PairChallenge → HelloProof → Paired,
    // with the token never transmitted (enroll_via_proof asserts Paired+confirm).
    let (addr, tmp) = start_server().await;
    let _ = enroll_via_proof(addr, tmp.path()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn already_paired_device_reconnects_without_a_window() {
    // Transitional resume: a device already in paired.json reconnects with no
    // active window (the shipped app re-sends Hello{token} every reconnect).
    let (addr, tmp) = start_server().await;
    let id = AgentIdentity::generate();
    let pk = encode_pubkey(&id.verifying_key());
    mur_core::mobile::add_paired_device(tmp.path(), &pk).unwrap();
    let mut ws = connect(addr).await;
    send_frame(
        &mut ws,
        &ClientFrame::Hello {
            pubkey: pk,
            token: "stale-or-empty".to_string(),
            agent: "mur".to_string(),
        },
    )
    .await;
    assert!(
        matches!(recv_server(&mut ws).await, ServerFrame::Paired { .. }),
        "already-paired device resumes by key without a window"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_reconnects_a_paired_device_via_challenge() {
    let (addr, tmp) = start_server().await;
    let id = AgentIdentity::generate();
    let pk = encode_pubkey(&id.verifying_key());
    mur_core::mobile::add_paired_device(tmp.path(), &pk).unwrap();

    let mut ws = connect(addr).await;
    send_frame(
        &mut ws,
        &ClientFrame::Resume {
            pubkey: pk.clone(),
            agent: "mur".to_string(),
        },
    )
    .await;
    let nonce = match recv_server(&mut ws).await {
        ServerFrame::Challenge { nonce } => nonce,
        other => panic!("expected Challenge, got {other:?}"),
    };
    // Sign exactly the issued nonce → proof accepted, session resumes by key.
    let proof = sign_payload(nonce.as_bytes().to_vec(), &id, 1);
    send_frame(&mut ws, &ClientFrame::ResumeProof { envelope: proof }).await;
    assert!(
        matches!(recv_server(&mut ws).await, ServerFrame::Paired { .. }),
        "valid resume proof reconnects the paired device"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_rejects_an_unpaired_device() {
    // The daemon issues a Challenge unconditionally (so it doesn't leak which
    // pubkeys are paired); an unpaired device can sign the nonce but still
    // fails resume_proof_ok (not in paired.json) → Rejected at the proof step.
    let (addr, _tmp) = start_server().await;
    let id = AgentIdentity::generate();
    let mut ws = connect(addr).await;
    send_frame(
        &mut ws,
        &ClientFrame::Resume {
            pubkey: encode_pubkey(&id.verifying_key()),
            agent: "mur".to_string(),
        },
    )
    .await;
    let nonce = match recv_server(&mut ws).await {
        ServerFrame::Challenge { nonce } => nonce,
        other => panic!("expected Challenge (no membership oracle), got {other:?}"),
    };
    let proof = sign_payload(nonce.as_bytes().to_vec(), &id, 1);
    send_frame(&mut ws, &ClientFrame::ResumeProof { envelope: proof }).await;
    assert!(
        matches!(recv_server(&mut ws).await, ServerFrame::Rejected { .. }),
        "a device not in paired.json cannot resume even with a valid signature"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_unpaired_envelope() {
    let (addr, tmp) = start_server().await;
    let (mut ws, _id) = enroll_via_proof(addr, tmp.path()).await;

    // An envelope signed by a DIFFERENT identity than the paired one.
    let id_b = AgentIdentity::generate();
    send_frame(
        &mut ws,
        &ClientFrame::Envelope {
            envelope: make_envelope(&id_b, "intrude"),
        },
    )
    .await;
    assert!(matches!(
        recv_server(&mut ws).await,
        ServerFrame::Rejected { .. }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn valid_envelope_mirrors_user_transcript() {
    let (addr, tmp) = start_server().await;
    let (mut ws, id) = enroll_via_proof(addr, tmp.path()).await;

    send_frame(
        &mut ws,
        &ClientFrame::Envelope {
            envelope: make_envelope(&id, "hello mur"),
        },
    )
    .await;

    // The user's turn is mirrored before the agent dial, so it appears
    // regardless of whether an agent is actually running.
    let path = tmp.path().join("agents/mur/mobile-events.jsonl");
    let mut found = false;
    for _ in 0..50 {
        if let Ok(s) = std::fs::read_to_string(&path)
            && s.contains("mobile.transcript")
            && s.contains("hello mur")
        {
            found = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(found, "expected mirrored transcript at {}", path.display());
}
