//! A remote endpoint the broker will push to, in its one canonical spelling.
//!
//! Two forms exist:
//! - `file:///absolute/path`: a repository on this machine.
//! - `https://<dns-name>[:<port>]/<path>`: a network remote.
//!
//! "One canonical spelling" is the rule for both: anything that could be written two ways
//! (`:443`, upper-case host, `%2e`, `127.1`, `//`) is refused rather than normalised, so the
//! stored string is also the digest input and two spellings can never be two enrollments.
//! The parser is a pure function: it resolves no name and opens no socket.
//!
//! Hosts are DNS names only, no IP literals. That is *not* an SSRF filter (`localhost`, or a
//! name that resolves to `169.254.169.254`, passes); it is there so exactly one spelling of
//! an address exists. See the https endpoint design, section 4.

use crate::constants::*;
use crate::error::BrokerError;
use serde::{Deserialize, Serialize};
use std::path::Path;

fn bad(m: impl Into<String>) -> BrokerError {
    BrokerError::InvalidRequest(m.into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Https,
    /// `http://127.0.0.1:<port>/...`, compiled only for tests: it carries the token in
    /// cleartext, which a shared machine's other local users could read by binding the port.
    #[cfg(any(test, feature = "test-support"))]
    LoopbackHttp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    raw: String,
    kind: Kind,
}

impl Endpoint {
    pub fn parse(s: &str) -> Result<Self, BrokerError> {
        let kind = if let Some(rest) = s.strip_prefix(ENDPOINT_FILE_SCHEME) {
            parse_file(rest)?;
            Kind::File
        } else if let Some(rest) = s.strip_prefix(ENDPOINT_HTTPS_SCHEME) {
            parse_https(rest)?;
            Kind::Https
        } else {
            #[cfg(any(test, feature = "test-support"))]
            if let Some(rest) = s.strip_prefix(ENDPOINT_LOOPBACK_HTTP_PREFIX) {
                parse_loopback_http(rest)?;
                return Ok(Self {
                    raw: s.to_string(),
                    kind: Kind::LoopbackHttp,
                });
            }
            return Err(bad("endpoint_scheme"));
        };
        Ok(Self {
            raw: s.to_string(),
            kind,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The filesystem path of the remote, for a `file://` endpoint. A network remote is not on
    /// this machine, so there is no path (callers must treat that as "not applicable", never
    /// as "fine").
    pub fn path(&self) -> Option<&Path> {
        match self.kind {
            Kind::File => Some(Path::new(&self.raw[ENDPOINT_FILE_SCHEME.len()..])),
            Kind::Https => None,
            #[cfg(any(test, feature = "test-support"))]
            Kind::LoopbackHttp => None,
        }
    }
}

impl Serialize for Endpoint {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.raw)
    }
}
impl<'de> Deserialize<'de> for Endpoint {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Endpoint::parse(&s).map_err(serde::de::Error::custom)
    }
}

/// `rest` is everything after `file://`.
fn parse_file(rest: &str) -> Result<(), BrokerError> {
    if !rest.starts_with('/') {
        return Err(bad("endpoint_scheme"));
    }
    // No percent-encoding, query or fragment: the path must mean exactly what it says.
    let clean = !rest.contains(['\0', '%', '?', '#', '\\'])
        && Path::new(rest).components().all(|c| {
            !matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
        && !rest.contains("//");
    if clean {
        Ok(())
    } else {
        Err(bad("endpoint_path"))
    }
}

/// `rest` is everything after `https://`: `<host>[:<port>]/<path>`.
fn parse_https(rest: &str) -> Result<(), BrokerError> {
    // The authority ends at the first `/`; no `/` at all means an empty path.
    let slash = rest.find('/').ok_or_else(|| bad("endpoint_path"))?;
    let (authority, path) = rest.split_at(slash);
    if authority.contains('@') {
        return Err(bad("endpoint_userinfo"));
    }
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    valid_host(host)?;
    if let Some(p) = port {
        let n = canonical_port(p)?;
        if n == HTTPS_DEFAULT_PORT {
            return Err(bad("endpoint_port"));
        }
    }
    valid_url_path(path)
}

/// `rest` is everything after `http://127.0.0.1:`: `<port>/<path>`.
#[cfg(any(test, feature = "test-support"))]
fn parse_loopback_http(rest: &str) -> Result<(), BrokerError> {
    let slash = rest.find('/').ok_or_else(|| bad("endpoint_path"))?;
    let (port, path) = rest.split_at(slash);
    if canonical_port(port)? == HTTP_DEFAULT_PORT {
        return Err(bad("endpoint_port"));
    }
    valid_url_path(path)
}

/// A port in its only spelling: ASCII digits, no leading zero, 1..=65535.
fn canonical_port(p: &str) -> Result<u16, BrokerError> {
    let digits = !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    if !digits || p.starts_with('0') {
        return Err(bad("endpoint_port"));
    }
    p.parse::<u16>().map_err(|_| bad("endpoint_port"))
}

/// A lowercase ASCII DNS name (punycode for anything international), and not a spelling of an
/// IP address.
fn valid_host(host: &str) -> Result<(), BrokerError> {
    if host.is_empty() || host.len() > ENDPOINT_HOST_MAX_LEN {
        return Err(bad("endpoint_host"));
    }
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.len() <= ENDPOINT_LABEL_MAX_LEN
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    // An empty label covers a leading dot, `..` and a trailing dot (the absolute-name form is
    // a second spelling).
    if !host.split('.').all(label_ok) {
        return Err(bad("endpoint_host"));
    }
    // Every IPv4 shorthand a URL parser would turn into an address ends in a numeric label:
    // `2130706433`, `127.1`, `0x7f.1`, `0x7f000001`, `0177.0.0.1`. A real top-level label is
    // never all digits, so refusing that closes the whole family at once.
    let last = host.rsplit('.').next().unwrap_or(host);
    let numeric = last.bytes().all(|b| b.is_ascii_digit());
    let hex = last
        .strip_prefix("0x")
        .is_some_and(|h| h.bytes().all(|b| b.is_ascii_hexdigit()));
    if numeric || hex {
        return Err(bad("endpoint_host"));
    }
    Ok(())
}

/// `/seg/seg`: ASCII `[A-Za-z0-9._~+-]` segments, none empty, none `.` or `..`, at least one.
/// An allow-list (design §4): anything not listed is refused, including whitespace, control
/// characters and non-ASCII, none of which a repository path needs.
fn valid_url_path(path: &str) -> Result<(), BrokerError> {
    let body = path.strip_prefix('/').ok_or_else(|| bad("endpoint_path"))?;
    let seg_ok = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'+' | b'-'))
    };
    if body.split('/').all(seg_ok) {
        Ok(())
    } else {
        Err(bad("endpoint_path"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(s: &str) {
        assert!(Endpoint::parse(s).is_ok(), "should parse: {s}");
    }
    fn no(s: &str) {
        assert!(Endpoint::parse(s).is_err(), "should be refused: {s}");
    }

    #[test]
    fn only_absolute_clean_file_urls_parse() {
        for s in ["file:///a", "file:///srv/x.git"] {
            ok(s);
        }
        for s in [
            "ssh://h/r",
            "file://host/r",
            "file://relative",
            "file:///a/../b",
            "file:///a//b",
            "file:///a%2e",
            "file:///a?x",
            "/plain",
            "",
        ] {
            no(s);
        }
    }

    #[test]
    fn https_accepts_the_canonical_spelling() {
        for s in [
            "https://git.example.com/org/repo.git",
            "https://git.example.com:8443/org/repo.git",
            "https://localhost/r.git",
            "https://gitserver/r.git",
            "https://xn--nxasmq6b.example/r.git",
            "https://a-b.c-d.example/r",
            "https://h:1/r",
            "https://h:65535/r",
            "https://h:442/r",
            "https://h:444/r",
            "https://h/a/b/c.git",
            "https://h/a.b/c-d_e~f",
        ] {
            ok(s);
        }
    }

    #[test]
    fn https_scheme_is_exactly_lowercase_https() {
        for s in [
            "HTTPS://h/r",
            "Https://h/r",
            "http://h/r",
            "http://h:80/r",
            "https:/h/r",
            "https:h/r",
            "https//h/r",
            "https:///r",
            "git://h/r",
            "ssh://h/r",
            "git+https://h/r",
        ] {
            no(s);
        }
    }

    #[test]
    fn https_userinfo_is_forbidden_anywhere_in_the_authority() {
        for s in [
            "https://user@h/r",
            "https://user:pw@h/r",
            "https://@h/r",
            "https://h@/r",
            "https://a@b@h/r",
            "https://h:443@evil.example/r",
            "https://good.example@evil.example/r",
        ] {
            no(s);
        }
    }

    #[test]
    fn https_host_is_a_lowercase_dns_name() {
        for s in [
            "https://GIT.example.com/r",
            "https://git.Example.com/r",
            "https://münchen.example/r",
            "https://gïț.example/r",
            "https://git_x.example/r",
            "https://git example.com/r",
            "https://git.example.com./r",
            "https://.example.com/r",
            "https://a..b/r",
            "https://-a.example/r",
            "https://a-.example/r",
            "https://a.-b.example/r",
            "https://:8443/r",
            "https:///r",
            "https://h*/r",
        ] {
            no(s);
        }
        let long_label = format!("https://{}.example/r", "a".repeat(64));
        no(&long_label);
        ok(&format!("https://{}.example/r", "a".repeat(63)));
        let long_host = format!("https://{}/r", vec!["a".repeat(60); 5].join("."));
        no(&long_host);
    }

    #[test]
    fn https_refuses_every_ip_literal_spelling() {
        for s in [
            "https://127.0.0.1/r",
            "https://10.0.0.5:8443/r",
            "https://169.254.169.254/r",
            "https://0.0.0.0/r",
            "https://2130706433/r",
            "https://127.1/r",
            "https://127.0.1/r",
            "https://0x7f.1/r",
            "https://0x7f000001/r",
            "https://0x7f.0x0.0x0.0x1/r",
            "https://0177.0.0.1/r",
            "https://[::1]/r",
            "https://[::1]:8443/r",
            "https://[2001:db8::1]/r",
            "https://::1/r",
            "https://1.2.3.4./r",
        ] {
            no(s);
        }
        // A digit inside a name is fine; only an all-numeric or 0x-hex last label is a number.
        ok("https://host1.example/r");
        ok("https://1host.example/r");
        ok("https://a.b1/r");
    }

    #[test]
    fn https_port_has_one_spelling() {
        for s in [
            "https://h:443/r",
            "https://h:0/r",
            "https://h:65536/r",
            "https://h:99999/r",
            "https://h:080/r",
            "https://h:00443/r",
            "https://h:/r",
            "https://h:+80/r",
            "https://h:-1/r",
            "https://h:8o/r",
            "https://h:8080:9090/r",
            "https://h: 80/r",
        ] {
            no(s);
        }
    }

    #[test]
    fn https_path_is_clean_ascii_and_non_empty() {
        for s in [
            "https://h",
            "https://h:8443",
            "https://h/",
            "https://h?x=1",
            "https://h#frag",
            "https://h/r/../s",
            "https://h/../r",
            "https://h/r/./s",
            "https://h/./r",
            "https://h/r/..",
            "https://h/r/.",
            "https://h//r",
            "https://h/r//s",
            "https://h/r%2e",
            "https://h/%2e%2e/r",
            "https://h/r?service=git-upload-pack",
            "https://h/r#x",
            "https://h/r\\s",
            "https://h/r\0s",
            "https://h/r s",
            "https://h/r\ns",
            "https://h/r\ts",
            "https://h/ré",
        ] {
            no(s);
        }
        // `..` or `.` as part of a longer name is just a name.
        ok("https://h/a..b/c.git");
        ok("https://h/.hidden/r");
        ok("https://h/r...");
    }

    #[test]
    fn https_has_no_path_on_this_machines_filesystem() {
        let e = Endpoint::parse("https://h/r.git").unwrap();
        assert_eq!(e.path(), None);
        let f = Endpoint::parse("file:///srv/r.git").unwrap();
        assert_eq!(f.path(), Some(Path::new("/srv/r.git")));
    }

    #[test]
    fn serialises_as_the_same_string_and_reloads() {
        for s in ["file:///srv/r.git", "https://git.example.com:8443/o/r.git"] {
            let e = Endpoint::parse(s).unwrap();
            assert_eq!(e.as_str(), s);
            let y = serde_yaml_ng::to_string(&e).unwrap();
            assert_eq!(y.trim(), s);
            let back: Endpoint = serde_yaml_ng::from_str(&y).unwrap();
            assert_eq!(back, e);
        }
    }

    #[test]
    fn a_hand_edited_file_with_a_bad_https_endpoint_fails_to_load() {
        for s in ["https://user@h/r", "https://H/r", "https://127.0.0.1/r"] {
            assert!(serde_yaml_ng::from_str::<Endpoint>(s).is_err(), "{s}");
        }
    }

    /// The loopback `http` row exists only for the tests (and `test-support`), never in a
    /// plain build: it sends the token in cleartext.
    #[test]
    fn loopback_http_is_the_test_seam_with_one_spelling() {
        for s in [
            "http://127.0.0.1:8080/r.git",
            "http://127.0.0.1:1/r",
            "http://127.0.0.1:65535/a/b",
        ] {
            ok(s);
            assert_eq!(Endpoint::parse(s).unwrap().path(), None);
        }
        for s in [
            "http://127.0.0.1/r",
            "http://127.0.0.1:80/r.git",
            "http://127.0.0.1:0/r",
            "http://127.0.0.1:08080/r",
            "http://127.0.0.1:65536/r",
            "http://localhost:8080/r",
            "http://127.0.0.2:8080/r",
            "http://0.0.0.0:8080/r",
            "http://[::1]:8080/r",
            "http://user@127.0.0.1:8080/r",
            "http://example.com:8080/r",
            "http://127.0.0.1:8080",
            "http://127.0.0.1:8080/",
            "http://127.0.0.1:8080/r/../s",
            "http://127.0.0.1:8080//r",
            "http://127.0.0.1:8080/r?x",
            "HTTP://127.0.0.1:8080/r",
        ] {
            no(s);
        }
    }
}
