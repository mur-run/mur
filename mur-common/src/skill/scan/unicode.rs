use unicode_normalization::UnicodeNormalization;

const BIDI_OVERRIDES: &[char] = &[
    '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}',
    '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
];

const INVISIBLE_SEPARATORS: &[char] = &[
    '\u{200B}', '\u{200C}', '\u{200D}',
    '\u{2060}',
    '\u{FEFF}',
];

#[derive(Debug, PartialEq, Eq)]
pub struct UnicodeFinding {
    pub kind: UnicodeKind,
    pub codepoint: char,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum UnicodeKind {
    BidiOverride,
    InvisibleSeparator,
    NotNfc,
}

pub fn scan_unicode(input: &str) -> (String, Vec<UnicodeFinding>) {
    let mut findings = Vec::new();
    let nfc: String = input.nfc().collect();
    if nfc != input {
        findings.push(UnicodeFinding { kind: UnicodeKind::NotNfc, codepoint: '\u{0}' });
    }
    for c in nfc.chars() {
        if BIDI_OVERRIDES.contains(&c) {
            findings.push(UnicodeFinding { kind: UnicodeKind::BidiOverride, codepoint: c });
        } else if INVISIBLE_SEPARATORS.contains(&c) {
            findings.push(UnicodeFinding { kind: UnicodeKind::InvisibleSeparator, codepoint: c });
        }
    }
    (nfc, findings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_ascii_passes() {
        let (n, f) = scan_unicode("hello world");
        assert_eq!(n, "hello world");
        assert!(f.is_empty());
    }

    #[test]
    fn detects_rlo() {
        let (_, f) = scan_unicode("hello\u{202E}world");
        assert!(f.iter().any(|x| x.kind == UnicodeKind::BidiOverride));
    }

    #[test]
    fn detects_zwj() {
        let (_, f) = scan_unicode("admin\u{200D}istrator");
        assert!(f.iter().any(|x| x.kind == UnicodeKind::InvisibleSeparator));
    }

    #[test]
    fn detects_non_nfc() {
        let nfd = "cafe\u{0301}";
        let (n, f) = scan_unicode(nfd);
        assert!(f.iter().any(|x| x.kind == UnicodeKind::NotNfc));
        assert_eq!(n, "café");
    }
}
