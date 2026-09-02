//! Bounded JSONL client for `codex app-server --listen stdio://`.
//!
//! One session per question: spawn, `initialize` + `initialized`, ask, kill.
//! Every request has a monotonically increasing id and a timeout; a session
//! that stops answering is killed and reaped rather than waited on. Only the
//! stable methods are used, so `experimentalApi` is never requested.

use super::{ChatGptAccountView, ChatGptModelView};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// A model page is a few KiB; anything past this is not a reply we want.
const MAX_LINE_BYTES: usize = 1 << 20;
const MODEL_PAGE_LIMIT: u64 = 100;
/// Bounds the pagination loop against a server that keeps handing out cursors.
const MAX_MODEL_PAGES: usize = 50;
const DEFAULT_INPUT_MODALITIES: [&str; 2] = ["text", "image"];

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("codex CLI not found on PATH")]
    CliMissing,
    #[error("could not start codex app-server: {0}")]
    Spawn(String),
    #[error("codex app-server closed the connection")]
    Eof,
    #[error("codex app-server did not answer within {0:?}")]
    Timeout(Duration),
    #[error("codex app-server error: {0}")]
    Rpc(String),
    #[error("unexpected reply from codex app-server: {0}")]
    Protocol(String),
}

struct JsonlSession {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    timeout: Duration,
}

impl JsonlSession {
    async fn start(codex: &Path) -> Result<Self, ControlError> {
        let mut child = Command::new(codex)
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // ponytail: stderr dropped — nothing here surfaces it; pipe it when a diagnostic needs it.
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ControlError::Spawn(e.to_string()))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut s = Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next_id: 0,
            timeout: REQUEST_TIMEOUT,
        };
        s.request(
            "initialize",
            json!({"clientInfo": {
                "name": "mur_hub",
                "title": "MUR Hub",
                "version": env!("CARGO_PKG_VERSION"),
            }}),
        )
        .await?;
        s.notify("initialized", json!({})).await?;
        Ok(s)
    }

    async fn write_line(&mut self, v: &Value) -> Result<(), ControlError> {
        let mut line = v.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|_| ControlError::Eof)
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), ControlError> {
        self.write_line(&json!({"method": method, "params": params}))
            .await
    }

    /// Send one request and wait for *its* reply, skipping notifications
    /// (lines without an `id`) and replies to other ids.
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, ControlError> {
        self.next_id += 1;
        let id = self.next_id;
        self.write_line(&json!({"id": id, "method": method, "params": params}))
            .await?;
        let timeout = self.timeout;
        tokio::time::timeout(timeout, self.await_reply(id))
            .await
            .map_err(|_| ControlError::Timeout(timeout))?
    }

    async fn await_reply(&mut self, id: u64) -> Result<Value, ControlError> {
        loop {
            let line = self
                .lines
                .next_line()
                .await
                .map_err(|e| ControlError::Protocol(e.to_string()))?
                .ok_or(ControlError::Eof)?;
            if line.len() > MAX_LINE_BYTES {
                return Err(ControlError::Protocol(format!(
                    "reply longer than {MAX_LINE_BYTES} bytes"
                )));
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue; // not JSON: a stray log line, not a reply
            };
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(err) = v.get("error") {
                let msg = err
                    .get("message")
                    .and_then(Value::as_str)
                    .map_or_else(|| err.to_string(), str::to_string);
                return Err(ControlError::Rpc(msg));
            }
            return v.get("result").cloned().ok_or_else(|| {
                ControlError::Protocol("reply has neither result nor error".into())
            });
        }
    }

    /// Kill and reap. `kill_on_drop` covers the error paths; this makes the
    /// happy path deterministic instead of leaving a zombie to the runtime.
    async fn close(mut self) {
        let _ = self.child.kill().await;
    }
}

pub async fn read_account(codex: &Path) -> Result<ChatGptAccountView, ControlError> {
    let mut s = JsonlSession::start(codex).await?;
    let r = s
        .request("account/read", json!({"refreshToken": false}))
        .await;
    s.close().await;
    Ok(account_view(&r?))
}

/// Only `account.type == "chatgpt"` is a subscription login. An API-key
/// login is reported (for diagnostics) but is *not* connected to this
/// provider — it bills OpenAI Platform.
fn account_view(v: &Value) -> ChatGptAccountView {
    let account = v.get("account").filter(|a| a.is_object());
    let auth_mode = account.and_then(|a| a["type"].as_str());
    let chatgpt = auth_mode == Some("chatgpt");
    let field = |k: &str| {
        chatgpt
            .then(|| account?.get(k)?.as_str().map(str::to_string))
            .flatten()
    };
    ChatGptAccountView {
        cli_present: true,
        logged_in: chatgpt,
        auth_mode: auth_mode.map(str::to_string),
        email: field("email"),
        plan_type: field("planType"),
    }
}

pub async fn list_models(codex: &Path) -> Result<Vec<ChatGptModelView>, ControlError> {
    let mut s = JsonlSession::start(codex).await?;
    let r = collect_models(&mut s).await;
    s.close().await;
    r
}

async fn collect_models(s: &mut JsonlSession) -> Result<Vec<ChatGptModelView>, ControlError> {
    let mut out = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut seen_cursors = HashSet::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_MODEL_PAGES {
        let mut params = json!({"limit": MODEL_PAGE_LIMIT, "includeHidden": false});
        if let Some(c) = &cursor {
            params["cursor"] = json!(c);
        }
        let page = s.request("model/list", params).await?;
        for m in page["data"].as_array().into_iter().flatten() {
            let Some(id) = m["model"].as_str().or_else(|| m["id"].as_str()) else {
                continue;
            };
            if seen_ids.insert(id.to_string()) {
                out.push(model_view(m, id));
            }
        }
        match page["nextCursor"].as_str().filter(|c| !c.is_empty()) {
            Some(c) if seen_cursors.insert(c.to_string()) => cursor = Some(c.to_string()),
            _ => break,
        }
    }
    Ok(out)
}

fn model_view(m: &Value, id: &str) -> ChatGptModelView {
    let strings = |v: &Value, key: &str| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e.as_str().or_else(|| e[key].as_str()).map(str::to_string))
            .collect()
    };
    let mut input_modalities = strings(&m["inputModalities"], "");
    if input_modalities.is_empty() {
        input_modalities = DEFAULT_INPUT_MODALITIES.map(String::from).to_vec();
    }
    ChatGptModelView {
        id: id.to_string(),
        display_name: m["displayName"].as_str().unwrap_or(id).to_string(),
        is_default: m["isDefault"].as_bool().unwrap_or(false),
        reasoning_efforts: strings(&m["supportedReasoningEfforts"], "reasoningEffort"),
        input_modalities,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;

    /// A fake `codex app-server`: logs every line it receives, answers by id.
    fn fake_codex(dir: &tempfile::TempDir, body: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let log = dir.path().join("requests.log");
        let script = dir.path().join("codex");
        let src = format!(
            "#!/bin/sh\nLOG='{}'\nwhile IFS= read -r line; do\n  printf '%s\\n' \"$line\" >> \"$LOG\"\n  id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\\([0-9]*\\).*/\\1/p')\n  [ -z \"$id\" ] && continue\n  case \"$line\" in\n{}\n  esac\ndone\n",
            log.display(),
            body
        );
        std::fs::File::create(&script)
            .unwrap()
            .write_all(src.as_bytes())
            .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (script, log)
    }

    const INIT_OK: &str =
        r#"    *'"initialize"'*) printf '{"id":%s,"result":{"userAgent":"codex/test"}}\n' "$id";;"#;

    #[tokio::test]
    async fn handshake_then_account_read_skips_notifications() {
        let dir = tempfile::tempdir().unwrap();
        let body = format!(
            "{INIT_OK}\n    *'\"account/read\"'*) printf '{{\"method\":\"thread/started\",\"params\":{{}}}}\\n'; printf '{{\"id\":99,\"result\":{{}}}}\\n'; printf '{{\"id\":%s,\"result\":{{\"account\":{{\"type\":\"chatgpt\",\"email\":\"u@example.com\",\"planType\":\"pro\"}}}}}}\\n' \"$id\";;"
        );
        let (codex, log) = fake_codex(&dir, &body);
        let view = read_account(&codex).await.unwrap();
        assert_eq!(
            view,
            ChatGptAccountView {
                cli_present: true,
                logged_in: true,
                auth_mode: Some("chatgpt".into()),
                email: Some("u@example.com".into()),
                plan_type: Some("pro".into()),
            }
        );
        let sent = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = sent.lines().collect();
        let init: Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(init["id"], 1);
        assert_eq!(init["method"], "initialize");
        assert_eq!(init["params"]["clientInfo"]["name"], "mur_hub");
        assert_eq!(init["params"]["clientInfo"]["title"], "MUR Hub");
        assert_eq!(
            init["params"]["clientInfo"]["version"],
            env!("CARGO_PKG_VERSION")
        );
        assert!(init["params"].get("experimentalApi").is_none());
        assert_eq!(lines[1], r#"{"method":"initialized","params":{}}"#);
        let read: Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(read["id"], 2);
        assert_eq!(read["method"], "account/read");
        assert_eq!(read["params"]["refreshToken"], false);
    }

    #[test]
    fn api_key_login_is_not_a_subscription() {
        let v = json!({"account": {"type": "apiKey", "email": "x@example.com"}});
        let view = account_view(&v);
        assert!(view.cli_present);
        assert!(!view.logged_in);
        assert_eq!(view.auth_mode.as_deref(), Some("apiKey"));
        assert_eq!(
            view.email, None,
            "an API-key identity is not this provider's"
        );
        let logged_out = account_view(&json!({"account": null}));
        assert!(!logged_out.logged_in);
        assert_eq!(logged_out.auth_mode, None);
    }

    #[tokio::test]
    async fn rpc_error_and_eof_and_timeout_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let body = format!(
            "{INIT_OK}\n    *'\"account/read\"'*) printf '{{\"id\":%s,\"error\":{{\"code\":-32000,\"message\":\"boom\"}}}}\\n' \"$id\";;"
        );
        let (codex, _) = fake_codex(&dir, &body);
        let err = read_account(&codex).await.err().unwrap();
        assert!(
            matches!(err, ControlError::Rpc(ref m) if m == "boom"),
            "{err}"
        );

        let dir = tempfile::tempdir().unwrap();
        let body = format!("{INIT_OK}\n    *'\"account/read\"'*) exit 0;;");
        let (codex, _) = fake_codex(&dir, &body);
        let err = read_account(&codex).await.err().unwrap();
        assert!(matches!(err, ControlError::Eof), "{err}");

        let dir = tempfile::tempdir().unwrap();
        let body = format!("{INIT_OK}\n    *'\"account/read\"'*) sleep 30;;");
        let (codex, _) = fake_codex(&dir, &body);
        let mut s = JsonlSession::start(&codex).await.unwrap();
        s.timeout = Duration::from_millis(300);
        let err = s.request("account/read", json!({})).await.err().unwrap();
        assert!(matches!(err, ControlError::Timeout(_)), "{err}");
        s.child.kill().await.unwrap();
        assert!(
            s.child.try_wait().unwrap().is_some(),
            "child reaped after kill"
        );

        let err = read_account(Path::new("/nonexistent/codex"))
            .await
            .err()
            .unwrap();
        assert!(matches!(err, ControlError::Spawn(_)), "{err}");
    }

    #[tokio::test]
    async fn model_list_paginates_dedups_and_defaults_modalities() {
        let dir = tempfile::tempdir().unwrap();
        let body = format!(
            concat!(
                "{}\n",
                "    *'\"model/list\"'*'\"cursor\"'*) printf '{{\"id\":%s,\"result\":{{\"data\":[",
                "{{\"id\":\"m2\",\"model\":\"gpt-5.6-mini\",\"displayName\":\"Mini\"}}],\"nextCursor\":null}}}}\\n' \"$id\";;\n",
                "    *'\"model/list\"'*) printf '{{\"id\":%s,\"result\":{{\"data\":[",
                "{{\"id\":\"m1\",\"model\":\"gpt-5.6-sol\",\"displayName\":\"Sol\",\"isDefault\":true,",
                "\"supportedReasoningEfforts\":[{{\"reasoningEffort\":\"low\"}},{{\"reasoningEffort\":\"high\"}}],",
                "\"inputModalities\":[\"text\"]}},{{\"id\":\"dup\",\"model\":\"gpt-5.6-sol\"}}],\"nextCursor\":\"p2\"}}}}\\n' \"$id\";;"
            ),
            INIT_OK
        );
        let (codex, log) = fake_codex(&dir, &body);
        let models = list_models(&codex).await.unwrap();
        assert_eq!(
            models,
            vec![
                ChatGptModelView {
                    id: "gpt-5.6-sol".into(),
                    display_name: "Sol".into(),
                    is_default: true,
                    reasoning_efforts: vec!["low".into(), "high".into()],
                    input_modalities: vec!["text".into()],
                },
                ChatGptModelView {
                    id: "gpt-5.6-mini".into(),
                    display_name: "Mini".into(),
                    is_default: false,
                    reasoning_efforts: vec![],
                    input_modalities: vec!["text".into(), "image".into()],
                },
            ]
        );
        let sent = std::fs::read_to_string(&log).unwrap();
        let pages: Vec<Value> = sent
            .lines()
            .filter(|l| l.contains("model/list"))
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0]["params"]["limit"], 100);
        assert_eq!(pages[0]["params"]["includeHidden"], false);
        assert!(pages[0]["params"].get("cursor").is_none());
        assert_eq!(pages[1]["params"]["cursor"], "p2");
    }
}
