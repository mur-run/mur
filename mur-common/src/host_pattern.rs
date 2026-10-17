//! The single write-time validator for host allow/deny entries (#1905).
//! Matching stays in `crate::net`; this only decides what may be STORED.

use std::net::IpAddr;

pub mod audit;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedHost {
    pub value: String,
    pub notes: Vec<Normalization>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Normalization {
    Lowercased { from: String },
    Punycode { from: String },
    LegacyDotWildcard,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostPatternError {
    Empty,
    Whitespace,
    UrlOrPath,
    HasPort,
    NoDot,
    BadChar(char),
    BadLabel(String),
    TooLong,
    BadWildcard,
}

impl std::fmt::Display for HostPatternError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "empty host"),
            Self::Whitespace => write!(f, "contains a space — one host per argument"),
            Self::UrlOrPath => write!(f, "contains a URL scheme or path — pass the bare host"),
            Self::HasPort => write!(f, "contains a port — pass the bare host"),
            Self::NoDot => write!(f, "has no dot — a host looks like `example.com`"),
            Self::BadChar(c) => write!(f, "contains `{c}`, which no hostname can contain"),
            Self::BadLabel(l) => write!(f, "label `{l}` is not a valid DNS label"),
            Self::TooLong => write!(f, "longer than 253 characters"),
            Self::BadWildcard => write!(
                f,
                "a wildcard is only allowed as the first label, e.g. `*.example.com`"
            ),
        }
    }
}

const SLASHES: [char; 3] = ['/', '／', '\\'];

pub fn normalize_host_pattern(raw: &str) -> Result<NormalizedHost, HostPatternError> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(HostPatternError::Empty);
    }
    if s.chars().any(char::is_whitespace) {
        return Err(HostPatternError::Whitespace);
    }
    if s.contains("://") || s.contains(SLASHES) {
        return Err(HostPatternError::UrlOrPath);
    }
    // IP literals first: bare IPv6 has colons that are not a port.
    let unbracketed = s
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .unwrap_or(s);
    if let Ok(ip) = unbracketed.parse::<IpAddr>() {
        // IPv6 is stored bracketed: both matchers compare exact strings, and
        // both see `[v6]` — the egress proxy's CONNECT authority host and
        // `Url::host_str`. A bare `::1` would never match (#1919 review).
        let value = match ip {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("[{v6}]"),
        };
        return Ok(NormalizedHost {
            value,
            notes: vec![],
        });
    }
    if s.contains(':') {
        return Err(HostPatternError::HasPort);
    }
    if s.eq_ignore_ascii_case("localhost") {
        return Ok(NormalizedHost {
            value: "localhost".into(),
            notes: lower_note(s),
        });
    }
    let mut notes = Vec::new();
    let (prefix, name) = if let Some(rest) = s.strip_prefix("*.") {
        ("*.", rest)
    } else if let Some(rest) = s.strip_prefix('.') {
        notes.push(Normalization::LegacyDotWildcard);
        (".", rest)
    } else {
        ("", s)
    };
    if name.contains('*') {
        return Err(HostPatternError::BadWildcard);
    }
    let ascii = if name.is_ascii() {
        name.to_ascii_lowercase()
    } else {
        // Refuse punctuation/symbols before IDNA so `評分，` names the comma.
        if let Some(c) = name
            .chars()
            .find(|c| !c.is_alphanumeric() && *c != '.' && *c != '-')
        {
            return Err(HostPatternError::BadChar(c));
        }
        let a = idna::domain_to_ascii_strict(name)
            .map_err(|_| HostPatternError::BadLabel(name.into()))?;
        notes.push(Normalization::Punycode { from: s.into() });
        a
    };
    if name.is_ascii() && ascii != name {
        notes.push(Normalization::Lowercased { from: s.into() });
    }
    if ascii.len() > 253 {
        return Err(HostPatternError::TooLong);
    }
    let labels: Vec<&str> = ascii.split('.').collect();
    if labels.len() < 2 {
        return Err(HostPatternError::NoDot);
    }
    for l in &labels {
        let ok = (1..=63).contains(&l.len())
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-');
        if !ok {
            return Err(HostPatternError::BadLabel((*l).into()));
        }
    }
    Ok(NormalizedHost {
        value: format!("{prefix}{ascii}"),
        notes,
    })
}

fn lower_note(s: &str) -> Vec<Normalization> {
    if s.bytes().any(|b| b.is_ascii_uppercase()) {
        vec![Normalization::Lowercased { from: s.into() }]
    } else {
        vec![]
    }
}
