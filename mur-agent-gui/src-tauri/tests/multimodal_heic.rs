use mur_agent_gui_lib::multimodal::heic::heic_to_png;

#[cfg(target_os = "macos")]
#[test]
fn heic_decodes_to_png_and_strips_metadata() {
    let heic = include_bytes!("fixtures/exif-gps.heic").to_vec();
    let png = heic_to_png(&heic).expect("HEIC decode");
    assert!(
        png.starts_with(&[0x89, 0x50, 0x4e, 0x47]),
        "PNG header present"
    );

    // The output PNG must NOT contain an EXIF chunk. PNG EXIF chunks
    // are tagged "eXIf" (lowercase except the X). image-rs strips them
    // on re-encode, but sips-piped output may pass them through — the
    // assertion catches both modes.
    let png_str = String::from_utf8_lossy(&png);
    assert!(
        !png_str.contains("eXIf"),
        "PNG should not contain EXIF chunk"
    );

    // If our fixture had GPS coords, also confirm those literal bytes
    // are absent. Use any GPS substring the fixture might carry; if
    // empty, the assertion is a no-op.
    if std::env::var("MUR_TEST_FIXTURE_HAS_GPS").is_ok() {
        assert!(!png_str.contains("37.4419"));
        assert!(!png_str.contains("-122.143"));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn non_heic_returns_error_or_passthrough() {
    // Feeding PNG bytes to heic_to_png should error cleanly (sips
    // refuses to convert PNG-as-HEIC, returning a non-zero exit code).
    let png = include_bytes!("fixtures/tiny.png").to_vec();
    assert!(heic_to_png(&png).is_err());
}

#[cfg(not(target_os = "macos"))]
#[test]
fn non_macos_returns_clear_error() {
    let heic = vec![0u8; 32];
    let err = mur_agent_gui_lib::multimodal::heic::heic_to_png(&heic).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("libheif")
            || err.to_string().to_lowercase().contains("not supported")
    );
}
