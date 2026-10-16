use super::*;

#[test]
fn accepts_and_normalizes() {
    for (raw, want) in [
        ("example.com", "example.com"),
        ("Shop.Example.COM", "shop.example.com"),
        ("xn--fiqs8s.tw", "xn--fiqs8s.tw"),
        ("例子.台灣", "xn--fsqu00a.xn--kpry57d"),
        ("*.x.com", "*.x.com"),
        (".x.com", ".x.com"),
        ("1.2.3.4", "1.2.3.4"),
        // IPv6 is stored bracketed: both matchers see `[v6]` (the proxy's
        // CONNECT authority and `Url::host_str`), and they compare exactly.
        ("2001:db8::1", "[2001:db8::1]"),
        ("[::1]", "[::1]"),
        ("::1", "[::1]"),
        ("localhost", "localhost"),
    ] {
        assert_eq!(normalize_host_pattern(raw).unwrap().value, want, "{raw}");
    }
}

#[test]
fn rejects_non_hosts() {
    for raw in [
        "",
        "   ",
        "pro360",
        "a",
        "591／tasker／鄰家師傅",
        "評分，",
        "ex ample.com",
        "a/b.com",
        "a／b.com",
        "https://x.com",
        "host:443",
        "*",
        "a.*.com",
        "*.com.",
        "-a.com",
        "a-.com",
        "a..com",
        &format!("{}.com", "a".repeat(64)),
        &format!("{}.com", "a.".repeat(130)),
    ] {
        assert!(
            normalize_host_pattern(raw).is_err(),
            "{raw:?} must be refused"
        );
    }
}

#[test]
fn records_every_normalization() {
    let n = normalize_host_pattern("例子.台灣").unwrap();
    assert!(
        n.notes
            .iter()
            .any(|x| matches!(x, Normalization::Punycode { from } if from == "例子.台灣"))
    );
    let n = normalize_host_pattern("A.com").unwrap();
    assert_eq!(
        n.notes,
        vec![Normalization::Lowercased {
            from: "A.com".into()
        }]
    );
    let n = normalize_host_pattern(".x.com").unwrap();
    assert_eq!(n.notes, vec![Normalization::LegacyDotWildcard]);
}
