use mur_agent_runtime::transport::noise::{FrameError, decode_frame, encode_frame};

#[test]
fn frame_roundtrip() {
    let payload = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let framed = encode_frame(payload).unwrap();
    // first 4 bytes = big-endian length
    assert_eq!(&framed[..4], &(payload.len() as u32).to_be_bytes());
    let decoded = decode_frame(&framed).unwrap();
    assert_eq!(decoded.payload, payload);
    assert_eq!(decoded.consumed, framed.len());
}

#[test]
fn short_header_errors() {
    let err = decode_frame(&[0, 0]).unwrap_err();
    assert!(matches!(err, FrameError::Incomplete));
}

#[test]
fn short_body_errors() {
    // Header says 100 bytes; only 5 follow
    let mut buf = vec![0, 0, 0, 100];
    buf.extend_from_slice(b"short");
    let err = decode_frame(&buf).unwrap_err();
    assert!(matches!(err, FrameError::Incomplete));
}

#[test]
fn oversize_rejected() {
    // 100 MB in header — refuse
    let buf = 100_000_000u32.to_be_bytes().to_vec();
    let err = decode_frame(&buf).unwrap_err();
    assert!(matches!(err, FrameError::TooLarge));
}
