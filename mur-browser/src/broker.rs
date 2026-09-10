//! Slice 5: fail-closed secret broker for the browser MCP proxy.
//!
//! The wire format intentionally matches browser-rs: one JSON request plus a
//! newline per Unix-socket connection, a capability token on every request,
//! and a distinct lease for the later redaction pass.  The broker never sends
//! secret values back to the proxy as metadata; they exist only in the
//! transformed tool request and in the broker's in-memory lease table.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[cfg(unix)]
use std::path::Path;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio::time::timeout;
use uuid::Uuid;

/// Keychain account prefix: `browser/<site>/<KEY>`.
pub const KEYCHAIN_PREFIX: &str = "browser/";
/// OS keyring service shared with `mur agent secret set`.
pub const KEYCHAIN_SERVICE: &str = "mur-agent";

/// Env var the proxy reads the token from, then `remove_var`s.
pub const TOKEN_ENV: &str = "MUR_BROWSER_BROKER_TOKEN";
/// Hard timeout per broker round-trip.
pub const TIMEOUT: Duration = Duration::from_secs(3);

/// A proxy-local broker connection. Keeping it behind a trait means the hook
/// can be tested without a socket and the proxy fails closed on every error.
pub trait BrokerClient: Send + Sync {
    fn transform<'a>(
        &'a self,
        value: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Transform>> + Send + 'a>>;
    fn redact<'a>(
        &'a self,
        lease: String,
        boundary: Option<Value>,
        value: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + 'a>>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Transform {
    pub value: Value,
    pub lease: String,
    pub boundary: Option<Value>,
}

/// Client for the one-request-per-connection Unix-socket wire protocol.
#[cfg(unix)]
#[derive(Debug, Clone)]
pub struct SocketClient {
    socket: std::path::PathBuf,
    token: String,
}

#[cfg(unix)]
impl SocketClient {
    pub fn new(socket: impl Into<std::path::PathBuf>, token: impl Into<String>) -> Self {
        Self {
            socket: socket.into(),
            token: token.into(),
        }
    }

    async fn request(&self, op: Op) -> Result<Response> {
        let stream = timeout(TIMEOUT, UnixStream::connect(&self.socket))
            .await
            .context("broker connection timed out")??;
        let (read, mut write) = stream.into_split();
        let request = Request {
            id: Uuid::new_v4().simple().to_string(),
            token: self.token.clone(),
            op,
        };
        let wire = serde_json::to_string(&request)?;
        timeout(TIMEOUT, async {
            write.write_all(wire.as_bytes()).await?;
            write.write_all(b"\n").await?;
            write.flush().await
        })
        .await
        .context("broker request timed out")??;
        let mut lines = BufReader::new(read).lines();
        let line = timeout(TIMEOUT, lines.next_line())
            .await
            .context("broker response timed out")??
            .ok_or_else(|| anyhow::anyhow!("broker closed without a response"))?;
        let response: Response = serde_json::from_str(&line).context("invalid broker response")?;
        if !response.ok {
            anyhow::bail!(
                response
                    .error
                    .unwrap_or_else(|| "broker rejected request".into())
            );
        }
        Ok(response)
    }
}

#[cfg(unix)]
impl BrokerClient for SocketClient {
    fn transform<'a>(
        &'a self,
        value: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Transform>> + Send + 'a>> {
        Box::pin(async move {
            let response = self.request(Op::TransformInput { value }).await?;
            Ok(Transform {
                value: response
                    .value
                    .ok_or_else(|| anyhow::anyhow!("broker transform omitted value"))?,
                lease: response
                    .lease
                    .ok_or_else(|| anyhow::anyhow!("broker transform omitted lease"))?,
                boundary: response.boundary,
            })
        })
    }

    fn redact<'a>(
        &'a self,
        lease: String,
        boundary: Option<Value>,
        value: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            let response = self
                .request(Op::RedactOutput {
                    lease,
                    boundary,
                    value,
                })
                .await?;
            response
                .value
                .ok_or_else(|| anyhow::anyhow!("broker redact omitted value"))
        })
    }
}

/// Keychain key prefix: `browser/<site>/<KEY>`.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Before a tool call: replace `{{secret:<site>/<KEY>}}` inside `value`.
    TransformInput { value: Value },
    /// After a tool call (success or error): scrub real values from `value`.
    RedactOutput {
        lease: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        boundary: Option<Value>,
        value: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Request {
    /// 32 hex, per request.
    pub id: String,
    pub token: String,
    #[serde(flatten)]
    pub op: Op,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// Required on `transform_input` success; correlates the redact step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease: Option<String>,
    /// Opaque; round-tripped untouched to `redact_output`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundary: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The narrow seam that lets `mur-core` adapt its OS Keychain without making
/// this lower-level crate depend on `mur-core` (which would create a cycle).
pub trait SecretStore: Send + Sync + 'static {
    fn get(&self, account: &str) -> Result<String>;
}

/// OS-native secret store. Accounts use `browser/<site>/<KEY>` under the
/// same `mur-agent` service used by `mur agent secret set`.
pub struct KeychainStore;

impl SecretStore for KeychainStore {
    fn get(&self, account: &str) -> Result<String> {
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, account)
            .with_context(|| format!("open Keychain entry {KEYCHAIN_SERVICE}:{account}"))?;
        match entry.get_password() {
            Ok(secret) if !secret.is_empty() => Ok(secret),
            Ok(_) => anyhow::bail!("Keychain entry {KEYCHAIN_SERVICE}:{account} is empty"),
            Err(keyring::Error::NoEntry) => {
                anyhow::bail!("Keychain entry {KEYCHAIN_SERVICE}:{account} does not exist")
            }
            Err(error) => Err(error)
                .with_context(|| format!("read Keychain entry {KEYCHAIN_SERVICE}:{account}")),
        }
    }
}

#[derive(Clone)]
struct LeaseSecret {
    key: String,
    value: String,
}

/// In-memory broker state. A broker is deliberately short lived: `record`
/// creates it, passes its token privately to the proxy, then removes its
/// socket when the recording process exits.
pub struct Broker {
    token: String,
    store: Arc<dyn SecretStore>,
    leases: Mutex<HashMap<String, Vec<LeaseSecret>>>,
}

impl Broker {
    pub fn new(token: impl Into<String>, store: Arc<dyn SecretStore>) -> Self {
        Self {
            token: token.into(),
            store,
            leases: Mutex::new(HashMap::new()),
        }
    }

    /// Process one decoded request. All invalid tokens, malformed placeholders,
    /// missing Keychain entries, and unknown/expired leases return `ok:false`;
    /// callers must not forward the browser tool call in those cases.
    pub fn handle(&self, request: Request) -> Response {
        if request.token != self.token {
            return Response::error("broker authentication failed");
        }
        match request.op {
            Op::TransformInput { value } => self.transform(value),
            Op::RedactOutput {
                lease,
                boundary: _,
                value,
            } => self.redact(&lease, value),
        }
    }

    fn transform(&self, mut value: Value) -> Response {
        let mut secrets = Vec::new();
        if let Err(error) = self.replace_placeholders(&mut value, &mut secrets) {
            return Response::error(error.to_string());
        }
        let lease = Uuid::new_v4().simple().to_string();
        let keys = secrets.iter().map(|s| s.key.clone()).collect::<Vec<_>>();
        if let Ok(mut leases) = self.leases.lock() {
            leases.insert(lease.clone(), secrets);
        } else {
            return Response::error("broker lease lock poisoned");
        }
        Response {
            ok: true,
            value: Some(value),
            lease: Some(lease),
            // This contains names only, never values; clients merely round it
            // back so the protocol remains browser-rs compatible.
            boundary: Some(json!({"keys": keys})),
            error: None,
        }
    }

    fn redact(&self, lease: &str, mut value: Value) -> Response {
        let secrets = match self.leases.lock() {
            Ok(mut leases) => leases.remove(lease), // one-shot lease: fail closed on reuse
            Err(_) => return Response::error("broker lease lock poisoned"),
        };
        let Some(secrets) = secrets else {
            return Response::error("unknown or expired broker lease");
        };
        redact_value(&mut value, &secrets);
        Response {
            ok: true,
            value: Some(value),
            lease: None,
            boundary: None,
            error: None,
        }
    }

    fn replace_placeholders(&self, value: &mut Value, found: &mut Vec<LeaseSecret>) -> Result<()> {
        match value {
            Value::String(text) => {
                let mut output = String::new();
                let mut rest = text.as_str();
                while let Some(start) = rest.find("{{secret:") {
                    output.push_str(&rest[..start]);
                    let after_prefix = &rest[start..];
                    let Some(end) = after_prefix.find("}}") else {
                        anyhow::bail!("unterminated secret placeholder");
                    };
                    let placeholder = &after_prefix[..end + 2];
                    let Some((site, key)) = parse_placeholder(placeholder) else {
                        anyhow::bail!("invalid secret placeholder {placeholder:?}");
                    };
                    let account = format!("{KEYCHAIN_PREFIX}{site}/{key}");
                    let secret = self
                        .store
                        .get(&account)
                        .with_context(|| format!("read Keychain entry {account:?}"))?;
                    output.push_str(&secret);
                    if !found
                        .iter()
                        .any(|item| item.key == key && item.value == secret)
                    {
                        found.push(LeaseSecret {
                            key: key.to_owned(),
                            value: secret,
                        });
                    }
                    rest = &after_prefix[end + 2..];
                }
                output.push_str(rest);
                *text = output;
            }
            Value::Array(items) => {
                for item in items {
                    self.replace_placeholders(item, found)?;
                }
            }
            Value::Object(map) => {
                for item in map.values_mut() {
                    self.replace_placeholders(item, found)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Serve connections until the listener is dropped or a transport error
    /// occurs. Each connection carries exactly one request and one response.
    /// The owner creates/removes the socket: silently deleting an existing
    /// path here could tear down another active recording session.
    #[cfg(unix)]
    pub async fn serve(self: Arc<Self>, socket: &Path) -> Result<()> {
        if socket.exists() {
            anyhow::bail!("broker socket already exists: {}", socket.display());
        }
        if let Some(parent) = socket.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(socket)
            .with_context(|| format!("bind broker socket {}", socket.display()))?;
        #[cfg(unix)]
        std::fs::set_permissions(socket, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        loop {
            let (stream, _) = listener.accept().await?;
            let broker = Arc::clone(&self);
            tokio::spawn(async move {
                let _ = broker.serve_connection(stream).await;
            });
        }
    }

    #[cfg(unix)]
    async fn serve_connection(&self, stream: UnixStream) -> Result<()> {
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        let Some(line) = timeout(TIMEOUT, lines.next_line())
            .await
            .context("broker request timed out")??
        else {
            return Ok(());
        };
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => self.handle(request),
            Err(error) => Response::error(format!("invalid broker request: {error}")),
        };
        let wire = serde_json::to_string(&response)?;
        timeout(TIMEOUT, async {
            write.write_all(wire.as_bytes()).await?;
            write.write_all(b"\n").await?;
            write.flush().await
        })
        .await
        .context("broker response timed out")??;
        Ok(())
    }
}

impl Response {
    fn error(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            value: None,
            lease: None,
            boundary: None,
            error: Some(error.into()),
        }
    }
}

/// Placeholder text → `(site, key)`.
pub fn parse_placeholder(s: &str) -> Option<(&str, &str)> {
    let inner = s.strip_prefix("{{secret:")?.strip_suffix("}}")?;
    let (site, key) = inner.split_once('/')?;
    let safe = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (safe(site) && safe(key)).then_some((site, key))
}

fn redact_value(value: &mut Value, secrets: &[LeaseSecret]) {
    match value {
        Value::String(text) => {
            for secret in secrets {
                if !secret.value.is_empty() {
                    *text = text.replace(&secret.value, &format!("[redacted:{}]", secret.key));
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_value(item, secrets);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                redact_value(item, secrets);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MemoryStore(Mutex<HashMap<String, String>>);
    impl MemoryStore {
        fn with(account: &str, value: &str) -> Self {
            Self(Mutex::new(HashMap::from([(account.into(), value.into())])))
        }
    }
    impl SecretStore for MemoryStore {
        fn get(&self, account: &str) -> Result<String> {
            self.0
                .lock()
                .unwrap()
                .get(account)
                .cloned()
                .context("missing test secret")
        }
    }
    fn broker() -> Broker {
        Broker::new(
            "t".repeat(32),
            Arc::new(MemoryStore::with("browser/pchome/PASSWORD", "Nope9Secret!")),
        )
    }
    fn request(op: Op) -> Request {
        Request {
            id: "0".repeat(32),
            token: "t".repeat(32),
            op,
        }
    }

    #[test]
    fn transform_then_redact_is_one_shot_and_never_returns_secret_metadata() {
        let b = broker();
        let transformed = b.handle(request(Op::TransformInput {
            value: json!({"text":"{{secret:pchome/PASSWORD}}"}),
        }));
        assert!(transformed.ok);
        assert_eq!(transformed.value.as_ref().unwrap()["text"], "Nope9Secret!");
        assert!(
            !transformed
                .boundary
                .as_ref()
                .is_some_and(|boundary| boundary.to_string().contains("Nope9Secret!"))
        );
        let lease = transformed.lease.unwrap();
        let redacted = b.handle(request(Op::RedactOutput {
            lease: lease.clone(),
            boundary: transformed.boundary,
            value: json!({"content":"typed Nope9Secret!"}),
        }));
        assert_eq!(
            redacted.value.unwrap()["content"],
            "typed [redacted:PASSWORD]"
        );
        assert!(
            !b.handle(request(Op::RedactOutput {
                lease,
                boundary: None,
                value: json!("x")
            }))
            .ok
        );
    }

    #[test]
    fn bad_token_and_missing_secret_fail_closed() {
        let b = broker();
        let mut bad = request(Op::TransformInput {
            value: json!("{{secret:pchome/PASSWORD}}"),
        });
        bad.token = "wrong".into();
        assert!(!b.handle(bad).ok);
        assert!(
            !b.handle(request(Op::TransformInput {
                value: json!("{{secret:pchome/MISSING}}")
            }))
            .ok
        );
    }

    #[test]
    fn keychain_store_uses_shared_service_and_browser_account_convention() {
        assert_eq!(KEYCHAIN_SERVICE, "mur-agent");
        assert_eq!(
            format!("{KEYCHAIN_PREFIX}pchome/PASSWORD"),
            "browser/pchome/PASSWORD"
        );
    }

    #[test]
    fn nested_placeholders_are_replaced_and_invalid_names_rejected() {
        let b = broker();
        let out = b.handle(request(Op::TransformInput {
            value: json!({"a":["x{{secret:pchome/PASSWORD}}y"]}),
        }));
        assert_eq!(out.value.unwrap()["a"][0], "xNope9Secret!y");
        assert_eq!(parse_placeholder("{{secret:pchome/a/b}}"), None);
        assert_eq!(parse_placeholder("{{secret:pchome/PASS WORD}}"), None);
    }
}
