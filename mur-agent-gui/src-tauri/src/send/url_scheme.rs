//! Track C3 channel A — `muragent-<slug>://share?...` deep-link
//! handler.
//!
//! On macOS the OS dispatches `muragent-coach://share?text=…&type=…`
//! URLs to the running `MyAgent.app` via Launch Services + the
//! [`tauri-plugin-deep-link`] plugin. This module owns the *parsing*
//! half: turn a raw URL string into a [`SharePayload`] with strict
//! validation (correct scheme prefix, correct host, base64-decoded
//! body). The deep-link plugin is wired into `lib.rs::setup` in
//! M-c3.1.3 — until then [`parse_share_url`] is exercised directly by
//! the test harness.

use anyhow::{Context, anyhow, ensure};
use base64::Engine;

use crate::send::{SharePayload, ShareKind};

/// Parse a `muragent-<expected_slug>://share?text=<base64>&type=<kind>`
/// URL into a [`SharePayload`].
///
/// Validation rules (fail-closed, in order):
/// 1. URL syntactically parseable.
/// 2. Scheme is exactly `muragent-<expected_slug>` — rejects URLs
///    targeting a different agent installed on the same machine.
/// 3. Host (the segment between `://` and the path/query) is `share`
///    — rejects future control verbs at non-`share` hosts.
/// 4. `text=<base64>` query parameter present and decodes as
///    `URL_SAFE_NO_PAD` UTF-8.
///
/// `type=` selects the [`ShareKind`] arm:
/// - `type=url` → [`ShareKind::Url`]
/// - anything else (including missing) → [`ShareKind::Text`]
pub fn parse_share_url(raw: &str, expected_slug: &str) -> anyhow::Result<SharePayload> {
    let parsed = url::Url::parse(raw).with_context(|| format!("invalid URL: {raw}"))?;
    let want_scheme = format!("muragent-{expected_slug}");
    ensure!(
        parsed.scheme() == want_scheme,
        "scheme mismatch: got `{}`, expected `{want_scheme}`",
        parsed.scheme()
    );
    ensure!(
        parsed.host_str() == Some("share"),
        "expected host `share`, got `{:?}`",
        parsed.host_str()
    );

    let mut text_b64: Option<String> = None;
    let mut kind_str = String::from("text");
    for (k, v) in parsed.query_pairs() {
        match k.as_ref() {
            "text" => text_b64 = Some(v.into_owned()),
            "type" => kind_str = v.into_owned(),
            _ => {}
        }
    }
    let b64 = text_b64.ok_or_else(|| anyhow!("missing `text=` query parameter"))?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(b64.as_bytes())
        .with_context(|| "decode `text=` as URL_SAFE_NO_PAD base64")?;
    let body = String::from_utf8(bytes).with_context(|| "decoded `text=` is not valid UTF-8")?;

    let kind = match kind_str.as_str() {
        "url" => ShareKind::Url(body),
        _ => ShareKind::Text(body),
    };
    Ok(SharePayload {
        source: "url_scheme".into(),
        kind,
        metadata: serde_json::json!({}),
    })
}
