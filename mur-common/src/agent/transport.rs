use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransportConfig {
    pub stdio: bool,
    pub socket: SocketTransportConfig,
    #[serde(default)]
    pub tcp: TcpTransportConfig,
    /// Track C5 — HTTP webhook receiver. Default off; enabling
    /// requires an HMAC secret in the OS keychain (`SecretRef`).
    /// See `docs/superpowers/specs/2026-05-05-mur-agent-c5-webhook-design.md`.
    #[serde(default)]
    pub webhook: WebhookTransportConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TcpTransportConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bind: String,
    #[serde(default)]
    pub noise: NoiseConfig,
}

/// HTTP webhook receiver — Track C5.
///
/// External systems POST `SharePayload`-shaped JSON to
/// `http://<bind>:<port>/agents/<slug>/webhook` with an
/// `X-Mur-Signature: sha256=<hex>` header carrying an HMAC-SHA256
/// over the raw body. The HMAC secret is stored in the OS keychain
/// via `SecretRef` (same pattern as Telegram bot tokens in C2);
/// `hmac_secret_ref` is the `service:account` lookup key.
///
/// `bind` defaults to `127.0.0.1` so a fresh enable doesn't
/// inadvertently expose the agent to the local network. Users who
/// want VPN / Tailscale reachability override to `0.0.0.0` or the
/// VPN interface address explicitly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WebhookTransportConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_webhook_bind")]
    pub bind: String,
    #[serde(default = "default_webhook_port")]
    pub port: u16,
    /// `service:account` key into the OS keychain. Empty string
    /// when `enabled = false`; required (and validated) at startup
    /// when enabled.
    #[serde(default)]
    pub hmac_secret_ref: String,
}

fn default_webhook_bind() -> String {
    "127.0.0.1".to_string()
}

fn default_webhook_port() -> u16 {
    6789
}

impl Default for WebhookTransportConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: default_webhook_bind(),
            port: default_webhook_port(),
            hmac_secret_ref: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoiseConfig {
    pub pattern: String,
}

impl Default for NoiseConfig {
    fn default() -> Self {
        Self {
            pattern: "Noise_XK_25519_ChaChaPoly_BLAKE2s".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SocketTransportConfig {
    pub enabled: bool,
    pub bind: String, // "unix:///path" or "tcp://host:port" (P0b)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuthConfig {
    pub scheme: String,
    pub token_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommunicationConfig {
    #[serde(default = "default_accepts_all")]
    pub accepts_from: Vec<String>,
    #[serde(default)]
    pub sends_to: Vec<String>,
}
fn default_accepts_all() -> Vec<String> {
    vec!["*".to_string()]
}
