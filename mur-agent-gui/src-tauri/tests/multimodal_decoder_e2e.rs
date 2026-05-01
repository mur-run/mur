//! End-to-end test for the `mur-agent-decoder` subprocess.
//!
//! Spawns the decoder bin, writes one `DecodeRequest` frame to stdin,
//! reads one `DecodeResponse` frame from stdout, asserts on the
//! decoded payload. Cargo populates `CARGO_BIN_EXE_mur-agent-decoder`
//! at test-build time so this works without hard-coding `target/`
//! paths.

use mur_agent_gui_lib::multimodal::decoder_protocol::{
    DecodeRequest, DecodeResponse, read_frame, write_frame,
};
use std::io::Write;
use std::process::{Command, Stdio};

fn decoder_path() -> String {
    env!("CARGO_BIN_EXE_mur-agent-decoder").to_string()
}

#[test]
fn decoder_handles_minimal_png() {
    // 1x1 transparent PNG (the "tiny PNG" trick — 67 bytes).
    let png_bytes = vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    let req = DecodeRequest::Image {
        bytes: png_bytes,
        mime_hint: "image/png".into(),
    };
    let req_bytes = serde_json::to_vec(&req).unwrap();

    let mut child = Command::new(decoder_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        write_frame(stdin, &req_bytes).unwrap();
        stdin.flush().unwrap();
    }
    let mut stdout = child.stdout.take().unwrap();
    let resp_bytes = read_frame(&mut stdout).unwrap();
    let resp: DecodeResponse = serde_json::from_slice(&resp_bytes).unwrap();
    child.wait().unwrap();

    match resp {
        DecodeResponse::Ok {
            png_bytes,
            decoder_version,
        } => {
            assert!(
                png_bytes.starts_with(&[0x89, 0x50, 0x4e, 0x47]),
                "PNG header"
            );
            assert!(decoder_version.contains("image-rs"));
        }
        other => panic!("expected Ok, got {other:?}"),
    }
}

#[test]
fn decoder_returns_error_on_garbage_bytes() {
    let req = DecodeRequest::Image {
        bytes: vec![0xde, 0xad, 0xbe, 0xef],
        mime_hint: "image/png".into(),
    };
    let req_bytes = serde_json::to_vec(&req).unwrap();
    let mut child = Command::new(decoder_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        write_frame(stdin, &req_bytes).unwrap();
        stdin.flush().unwrap();
    }
    let mut stdout = child.stdout.take().unwrap();
    let resp_bytes = read_frame(&mut stdout).unwrap();
    let resp: DecodeResponse = serde_json::from_slice(&resp_bytes).unwrap();
    child.wait().unwrap();

    assert!(
        matches!(resp, DecodeResponse::Error(_)),
        "expected Error, got {resp:?}"
    );
}
