use crate::content::*;

#[test]
fn plain_text_with_tab_newline_cr_passes() {
    assert_eq!(check_content(b"a\tb\r\nc").unwrap(), "a\tb\r\nc");
    assert_eq!(
        check_content("héllo 世界".as_bytes()).unwrap(),
        "héllo 世界"
    );
}

#[test]
fn invalid_utf8_is_rejected() {
    assert_eq!(check_content(b"\xff\xfe abc"), Err(ContentError::NotUtf8));
}

#[test]
fn nul_esc_and_c1_are_rejected_in_content() {
    for bad in ["a\0b", "a\x1b[31mred", "a\u{85}b", "a\u{9f}b", "\x08"] {
        assert_eq!(
            check_content(bad.as_bytes()),
            Err(ContentError::ControlChar),
            "{bad:?}"
        );
    }
}

#[test]
fn nul_esc_and_c1_are_rejected_in_summary() {
    for bad in ["a\0b", "a\x1b[31mred", "a\u{85}b", "a\u{9f}b"] {
        assert_eq!(
            check_summary(bad),
            Err(ContentError::ControlChar),
            "{bad:?}"
        );
    }
    assert_eq!(check_summary("ok\tsummary\n"), Ok(()));
}

#[test]
fn archive_magic_is_rejected() {
    for magic in ARCHIVE_MAGICS {
        let mut bytes = magic.to_vec();
        bytes.extend_from_slice(b"rest");
        assert!(check_content(&bytes).is_err(), "{magic:?}");
    }
    assert_eq!(check_content(b"PK\x03\x04zip"), Err(ContentError::Archive));
}

#[test]
fn text_that_merely_mentions_an_archive_passes() {
    assert!(check_content(b"the zip header is PK").is_ok());
}

#[test]
fn summary_limit_is_in_bytes() {
    assert_eq!(check_summary(&"a".repeat(MAX_SUMMARY_BYTES)), Ok(()));
    assert_eq!(
        check_summary(&"a".repeat(MAX_SUMMARY_BYTES + 1)),
        Err(ContentError::SummaryTooLong)
    );
    // 171 three-byte characters are 513 bytes but only 171 chars.
    assert_eq!(
        check_summary(&"世".repeat(171)),
        Err(ContentError::SummaryTooLong)
    );
}
