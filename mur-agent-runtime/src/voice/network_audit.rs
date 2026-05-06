//! Voice network-egress privacy audit.
//!
//! Mirrors `companion/network_audit.rs`. Fails the build (at test time)
//! if any voice module imports a network client.

#[cfg(test)]
const VOICE_FILES: &[(&str, &str)] = &[
    ("mod.rs", include_str!("mod.rs")),
    ("types.rs", include_str!("types.rs")),
    ("download.rs", include_str!("download.rs")),
    ("tts.rs", include_str!("tts.rs")),
    ("stt.rs", include_str!("stt.rs")),
    ("audio.rs", include_str!("audio.rs")),
    ("notifier.rs", include_str!("notifier.rs")),
    // network_audit.rs itself is intentionally omitted — it declares the
    // forbidden tokens as string literals and would trigger a false positive.
];

#[cfg(test)]
const FORBIDDEN_TOKENS: &[&str] = &[
    "use reqwest",
    "use hyper",
    "use surf",
    "use ureq",
    "use isahc",
    "use tokio::net::",
    "::TcpStream",
    "::TcpListener",
    "::UdpSocket",
    "std::net::TcpStream",
    "std::net::TcpListener",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_modules_have_no_network_egress() {
        for (filename, src) in VOICE_FILES {
            for token in FORBIDDEN_TOKENS {
                assert!(
                    !src.contains(token),
                    "voice/{filename} contains forbidden network token {token:?}"
                );
            }
        }
    }
}
