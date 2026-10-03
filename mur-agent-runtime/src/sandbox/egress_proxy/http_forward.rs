//! Request-head classification for the egress proxy (#1677): `CONNECT`
//! tunnels, absolute-form plain-HTTP forwards, and everything else.
//!
//! Pure string work so the parsing is unit-tested without sockets; the proxy
//! applies the same token + allowlist + SSRF screen to both tunnel kinds.

/// Port assumed for an `http://` URL that names none.
const HTTP_DEFAULT_PORT: u16 = 80;

/// Hop-by-hop / proxy-only headers never forwarded upstream. `connection` is
/// replaced by our own `Connection: close` (see [`rewrite_head`]).
const STRIPPED_HEADERS: [&str; 4] = [
    "proxy-authorization",
    "proxy-connection",
    "connection",
    "keep-alive",
];

/// What the client asked the proxy to do.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Classified {
    /// `CONNECT host:port` — tunnel bytes after a `200`.
    Connect { target: String },
    /// `GET http://host[:port]/path` — dial `target`, send `head` (rewritten
    /// to origin-form, proxy credentials stripped), then relay.
    Forward { target: String, head: String },
    /// Anything else, with a reason the client and the log both get.
    Unsupported(&'static str),
}

impl Classified {
    /// Label for the audit line: `egress proxy <kind> ALLOW|DENY|CHALLENGE`.
    pub(super) fn kind(&self) -> &'static str {
        match self {
            Classified::Connect { .. } => "CONNECT",
            Classified::Forward { .. } => "HTTP",
            Classified::Unsupported(_) => "UNSUPPORTED",
        }
    }
}

/// Classify a full request head (request line + headers, CRLF-terminated).
pub(super) fn classify(head: &str) -> Classified {
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let (Some(method), Some(uri), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Classified::Unsupported("malformed request line");
    };
    if method == "CONNECT" {
        return Classified::Connect {
            target: uri.to_string(),
        };
    }
    let Some(scheme_end) = uri.find("://") else {
        return Classified::Unsupported(
            "request is not absolute-form (`GET http://host/path`); send it through the proxy as a proxy request",
        );
    };
    if !uri[..scheme_end].eq_ignore_ascii_case("http") {
        return Classified::Unsupported(
            "only http:// is forwarded as a plain request; use CONNECT for https://",
        );
    }
    let rest = &uri[scheme_end + 3..];
    let split = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    // Userinfo (`user@host`) never decides the destination.
    let authority = rest[..split].rsplit('@').next().unwrap_or_default();
    if authority.is_empty() {
        return Classified::Unsupported("absolute-form URL has no host");
    }
    let path = match &rest[split..] {
        "" => "/".to_string(),
        p if p.starts_with('/') => p.to_string(),
        p => format!("/{p}"),
    };
    // Fragments are client-side only; never send one upstream.
    let path = path.split('#').next().unwrap_or("/").to_string();
    Classified::Forward {
        target: with_default_port(authority),
        head: rewrite_head(lines, method, &path, version, authority),
    }
}

/// `host` → `host:80`; `host:p` and `[v6]:p` unchanged; `[v6]` → `[v6]:80`.
fn with_default_port(authority: &str) -> String {
    let has_port = match authority.rfind(']') {
        Some(close) => authority[close..].contains(':'),
        None => authority.contains(':'),
    };
    if has_port {
        authority.to_string()
    } else {
        format!("{authority}:{HTTP_DEFAULT_PORT}")
    }
}

/// Origin-form request line + the client's headers minus [`STRIPPED_HEADERS`],
/// a `Host` if the client sent none, and `Connection: close`. Closing after
/// one response matters: a kept-alive connection would carry the client's
/// next request — possibly for another host — to this already-vetted upstream.
fn rewrite_head<'a>(
    headers: impl Iterator<Item = &'a str>,
    method: &str,
    path: &str,
    version: &str,
    authority: &str,
) -> String {
    let mut out = format!("{method} {path} {version}\r\n");
    let mut has_host = false;
    for line in headers.filter(|l| !l.is_empty()) {
        let name = line.split_once(':').map(|(n, _)| n.trim()).unwrap_or(line);
        if STRIPPED_HEADERS
            .iter()
            .any(|h| name.eq_ignore_ascii_case(h))
        {
            continue;
        }
        has_host |= name.eq_ignore_ascii_case("host");
        out.push_str(line);
        out.push_str("\r\n");
    }
    if !has_host {
        out.push_str(&format!("Host: {authority}\r\n"));
    }
    out.push_str("Connection: close\r\n\r\n");
    out
}

/// The `501` for [`Classified::Unsupported`]: says why, so the failure is
/// diagnosable from the client side too.
pub(super) fn unsupported_response(reason: &str) -> String {
    let body = format!("MUR egress proxy: {reason}\n");
    format!(
        "HTTP/1.1 501 Not Implemented\r\nContent-Type: text/plain\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forward(head: &str) -> (String, String) {
        match classify(head) {
            Classified::Forward { target, head } => (target, head),
            other => panic!("expected Forward, got {other:?}"),
        }
    }

    #[test]
    fn connect_keeps_its_authority() {
        assert_eq!(
            classify("CONNECT example.com:443 HTTP/1.1\r\n\r\n"),
            Classified::Connect {
                target: "example.com:443".into()
            }
        );
    }

    #[test]
    fn absolute_form_is_rewritten_to_origin_form() {
        let (target, head) = forward(
            "GET http://example.com/a/b?q=1#frag HTTP/1.1\r\n\
             Host: example.com\r\n\
             proxy-authorization: Basic dG9rOng=\r\n\
             Proxy-Connection: keep-alive\r\n\
             Connection: keep-alive\r\n\
             Accept: */*\r\n\r\n",
        );
        assert_eq!(target, "example.com:80");
        assert_eq!(
            head,
            "GET /a/b?q=1 HTTP/1.1\r\nHost: example.com\r\nAccept: */*\r\nConnection: close\r\n\r\n"
        );
    }

    #[test]
    fn explicit_port_empty_path_and_missing_host_header() {
        let (target, head) = forward("POST http://Example.com:8080 HTTP/1.1\r\n\r\n");
        assert_eq!(target, "Example.com:8080");
        assert_eq!(
            head,
            "POST / HTTP/1.1\r\nHost: Example.com:8080\r\nConnection: close\r\n\r\n"
        );
    }

    #[test]
    fn query_only_gets_a_leading_slash_and_userinfo_is_dropped() {
        let (target, head) = forward("GET http://u:p@example.com?x=1 HTTP/1.1\r\n\r\n");
        assert_eq!(target, "example.com:80");
        assert!(head.starts_with("GET /?x=1 HTTP/1.1\r\n"), "{head}");
    }

    #[test]
    fn ipv6_literal_gets_the_default_port() {
        assert_eq!(with_default_port("[::1]"), "[::1]:80");
        assert_eq!(with_default_port("[::1]:8080"), "[::1]:8080");
    }

    #[test]
    fn non_http_or_origin_form_or_garbage_is_unsupported() {
        for head in [
            "GET https://example.com/ HTTP/1.1\r\n\r\n",
            "GET /index.html HTTP/1.1\r\n\r\n",
            "GET http:///path HTTP/1.1\r\n\r\n",
            "garbage\r\n\r\n",
        ] {
            assert!(
                matches!(classify(head), Classified::Unsupported(_)),
                "{head:?}"
            );
        }
    }

    #[test]
    fn unsupported_response_is_self_delimiting_and_says_why() {
        let r = unsupported_response("nope");
        assert!(r.starts_with("HTTP/1.1 501 Not Implemented\r\n"), "{r}");
        assert!(r.ends_with("\r\n\r\nMUR egress proxy: nope\n"), "{r}");
        assert!(r.contains("Content-Length: 23\r\n"), "{r}");
    }
}
