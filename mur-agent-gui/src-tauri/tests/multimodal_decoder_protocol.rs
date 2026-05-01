use mur_agent_gui_lib::multimodal::decoder_protocol::{
    DecodeError, DecodeRequest, DecodeResponse, PdfPageText, read_frame, write_frame,
};

#[test]
fn request_response_roundtrip() {
    let req = DecodeRequest::Image {
        bytes: vec![0xff, 0xd8, 0xff, 0xe0],
        mime_hint: "image/jpeg".into(),
    };
    let s = serde_json::to_string(&req).unwrap();
    let back: DecodeRequest = serde_json::from_str(&s).unwrap();
    assert!(matches!(back, DecodeRequest::Image { .. }));

    let resp_ok = DecodeResponse::Ok {
        png_bytes: vec![0x89, 0x50, 0x4e, 0x47],
        decoder_version: "image-rs/0.25 + libheif-rs/1.0".into(),
    };
    assert!(
        serde_json::to_string(&resp_ok)
            .unwrap()
            .contains("\"type\":\"ok\"")
    );

    let resp_err = DecodeResponse::Error(DecodeError::UnsupportedFormat {
        mime: "application/octet-stream".into(),
    });
    let err_str = serde_json::to_string(&resp_err).unwrap();
    assert!(err_str.contains("unsupported_format"));
}

#[test]
fn pdf_text_response() {
    let resp = DecodeResponse::PdfText {
        pages: vec![
            PdfPageText {
                page: 0,
                text: "Page 1 content".into(),
                quarantined: false,
            },
            PdfPageText {
                page: 1,
                text: "Page 2 content".into(),
                quarantined: true,
            },
        ],
        decoder_version: "pdfium/5.0".into(),
    };
    let s = serde_json::to_string(&resp).unwrap();
    assert!(s.contains("\"type\":\"pdf_text\""));
    assert!(s.contains("\"page\":0"));
    assert!(s.contains("\"page\":1"));
    assert!(s.contains("\"quarantined\":true"));

    let back: DecodeResponse = serde_json::from_str(&s).unwrap();
    match back {
        DecodeResponse::PdfText { pages, .. } => {
            assert_eq!(pages.len(), 2);
            assert_eq!(pages[0].page, 0);
            assert_eq!(pages[1].quarantined, true);
        }
        _ => panic!("expected PdfText"),
    }
}

#[test]
fn frame_roundtrip() {
    let payload = b"hello world";
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, payload).unwrap();
    // First 4 bytes are big-endian length.
    assert_eq!(&buf[..4], &(payload.len() as u32).to_be_bytes());
    assert_eq!(&buf[4..], payload);
    let mut cursor = std::io::Cursor::new(buf);
    let read_back = read_frame(&mut cursor).unwrap();
    assert_eq!(read_back, payload);
}

#[test]
fn frame_too_large_errors() {
    // 65 MiB length > 64 MiB cap.
    let huge: u32 = 65 * 1024 * 1024 + 1;
    let bytes = huge.to_be_bytes();
    let mut cursor = std::io::Cursor::new(bytes.to_vec());
    let err = read_frame(&mut cursor).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn frame_empty_payload() {
    let payload = b"";
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, payload).unwrap();
    assert_eq!(&buf[..4], &0u32.to_be_bytes());
    assert_eq!(buf.len(), 4);
    let mut cursor = std::io::Cursor::new(buf);
    let read_back = read_frame(&mut cursor).unwrap();
    assert_eq!(read_back, b"");
}

#[test]
fn frame_large_payload() {
    let payload = vec![0x42u8; 1024 * 1024]; // 1 MiB
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &payload).unwrap();
    assert_eq!(&buf[..4], &(payload.len() as u32).to_be_bytes());
    assert_eq!(&buf[4..], payload.as_slice());
    let mut cursor = std::io::Cursor::new(buf);
    let read_back = read_frame(&mut cursor).unwrap();
    assert_eq!(read_back, payload);
}
