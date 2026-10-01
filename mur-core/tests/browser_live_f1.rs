//! F1 / F2 end-to-end for browser live mode, at the MCP tool level.
//!
//! Opt-in: set `MUR_BROWSER_E2E=1`. Everything else skips, because this needs
//! a real Chromium outside any agent seal and the pinned `@playwright/mcp`
//! (both from `mur browser setup`), `node`, `python3`, and `openssl` for the
//! fixture's self-signed cert.
//!
//! What runs: the real egress proxy (`mur-agent-runtime`), two loopback HTTPS
//! shops (`scripts/e2e/browser-live-fixture.py`), and the real
//! `mur browser record --mode live` child with `HTTPS_PROXY` set exactly the
//! way the runtime sets it. The test then speaks MCP to the child the way an
//! agent would. The LLM step (an agent reading the prices and naming the
//! cheaper shop) is `scripts/e2e/browser-live-f1.sh`.
//!
//! Inside a sandboxed host, pass Chromium `--no-sandbox` through
//! `MUR_BROWSER_CHROMIUM_ARGS`; the test forwards it untouched.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

const GATE: &str = "MUR_BROWSER_E2E";
const STEP_TIMEOUT: Duration = Duration::from_secs(90);

fn gated() -> bool {
    if std::env::var_os(GATE).is_some_and(|v| !v.is_empty() && v != "0") {
        return true;
    }
    eprintln!("skipped: set {GATE}=1 to run the browser live-mode e2e");
    false
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("mur-core sits inside the workspace")
        .to_path_buf()
}

/// The two shops, as announced by the fixture on its first stdout line.
struct Fixture {
    child: Child,
    announce: Value,
}

impl Fixture {
    async fn start() -> Self {
        let script = repo_root().join("scripts/e2e/browser-live-fixture.py");
        let mut child = Command::new("python3")
            .arg(&script)
            .arg("--parent-pid")
            .arg(std::process::id().to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {}: {e}", script.display()));
        let stdout = child.stdout.take().expect("piped");
        let line = tokio::time::timeout(STEP_TIMEOUT, BufReader::new(stdout).lines().next_line())
            .await
            .expect("fixture announced itself in time")
            .expect("fixture stdout readable")
            .expect("fixture printed its announcement");
        let announce: Value = serde_json::from_str(&line).expect("announcement is JSON");
        Self { child, announce }
    }

    fn url(&self, shop: &str) -> String {
        self.announce["shops"][shop]["url"]
            .as_str()
            .expect("shop url")
            .to_owned()
    }

    fn price(&self, shop: &str) -> &str {
        self.announce["shops"][shop]["price"]
            .as_str()
            .expect("shop price")
    }

    fn port(&self, shop: &str) -> u64 {
        self.announce["shops"][shop]["port"]
            .as_u64()
            .expect("shop port")
    }

    fn cheaper(&self) -> &str {
        self.announce["cheaper"].as_str().expect("cheaper")
    }

    async fn stop(mut self) {
        let _ = self.child.kill().await;
    }
}

/// The real `mur browser record --mode live` child, driven over MCP stdio.
struct LiveBrowser {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

impl LiveBrowser {
    async fn launch(mur_home: &std::path::Path, proxy_url: &str) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_mur"));
        cmd.args(["browser", "record", "--run", "live-f1", "--mode", "live"])
            // Trailing args go verbatim to @playwright/mcp. The fixture's
            // cert is self-signed; nothing else about TLS is relaxed.
            .args(["--", "--ignore-https-errors"])
            .env("MUR_HOME", mur_home)
            // Byte-for-byte what `proxy_env_for` hands a Restricted entry.
            .env("HTTP_PROXY", proxy_url)
            .env("HTTPS_PROXY", proxy_url)
            .env("http_proxy", proxy_url)
            .env("https_proxy", proxy_url)
            .env("NO_PROXY", "127.0.0.1,localhost,::1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = cmd.spawn().expect("spawn mur browser record");
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let mut me = Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next_id: 1,
        };
        me.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "browser_live_f1", "version": "0" }
            }),
        )
        .await;
        me.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}))
            .await;
        me
    }

    async fn send(&mut self, message: &Value) {
        let mut line = serde_json::to_vec(message).expect("serialize");
        line.push(b'\n');
        self.stdin.write_all(&line).await.expect("write to child");
        self.stdin.flush().await.expect("flush");
    }

    /// Returns the whole response object (`result` or `error` present).
    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = format!("f1-{}", self.next_id);
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        tokio::time::timeout(STEP_TIMEOUT, async {
            while let Some(line) = self.lines.next_line().await.expect("read child") {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if message.get("id").and_then(Value::as_str) == Some(id.as_str()) {
                    return message;
                }
            }
            panic!("child closed stdout before answering {method} ({id})");
        })
        .await
        .unwrap_or_else(|_| panic!("{method} ({id}) timed out"))
    }

    async fn call(&mut self, tool: &str, args: Value) -> Value {
        self.request("tools/call", json!({"name": tool, "arguments": args}))
            .await
    }

    async fn stop(mut self) {
        drop(self.stdin);
        let _ = tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await;
        let _ = self.child.kill().await;
    }
}

/// The child launches `node <install>/cli.js` from `<MUR_HOME>/browser/mcp-server`
/// and has no `npx` fallback, so the temp home needs the pinned server. Link
/// the one `mur browser setup` installed on this machine rather than running
/// npm here: the test is about the proxy, not the install.
fn seed_server_install(mur_home: &std::path::Path) {
    let system_home = mur_browser::paths::system_mur_home().expect("locate the MUR home");
    let src = mur_browser::server::install_dir(&system_home);
    assert!(
        mur_browser::server::installed_entry(&src).is_some(),
        "{}",
        mur_browser::server::missing_install_error(&src)
    );
    let dst = mur_browser::server::install_dir(mur_home);
    std::fs::create_dir_all(dst.parent().expect("versioned dir has a parent"))
        .expect("create <home>/browser/mcp-server");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&src, &dst).expect("link the installed server");
    #[cfg(not(unix))]
    panic!("browser live e2e is unix-only");
}

/// All text parts of a tool result, joined.
fn text_of(response: &Value) -> String {
    let mut out = String::new();
    if let Some(parts) = response["result"]["content"].as_array() {
        for part in parts {
            if let Some(t) = part["text"].as_str() {
                out.push_str(t);
                out.push('\n');
            }
        }
    }
    if let Some(err) = response.get("error") {
        out.push_str(&err.to_string());
    }
    if response["result"]["isError"].as_bool() == Some(true) {
        out.push_str("\n[isError]");
    }
    out
}

fn is_tool_error(response: &Value) -> bool {
    response.get("error").is_some() || response["result"]["isError"].as_bool() == Some(true)
}

/// F1 at the tool level, plus the one F2 case that proves the proxy is in the
/// path at all: the allowlist is `127.0.0.1`, so `localhost` must be denied by
/// name even though it is the same machine. Without that assertion a run with
/// no proxy whatsoever would also "pass" F1.
#[tokio::test]
async fn f1_two_shops_both_prices_read_through_the_egress_proxy() {
    if !gated() {
        return;
    }
    let mur_home = tempfile::tempdir().expect("tempdir");
    seed_server_install(mur_home.path());
    let fixture = Fixture::start().await;

    let proxy = mur_agent_runtime::sandbox::egress_proxy::start_egress_proxy("f1")
        .await
        .expect("egress proxy binds 127.0.0.1:0");
    let token = proxy.register(vec!["127.0.0.1".to_owned()]);
    let proxy_url = format!("http://{token}:x@{}", proxy.addr);

    let mut browser = LiveBrowser::launch(mur_home.path(), &proxy_url).await;

    // --- F1: both shops, both prices ---
    let mut seen = Vec::new();
    for shop in ["starling", "magpie"] {
        let nav = browser
            .call("browser_navigate", json!({"url": fixture.url(shop)}))
            .await;
        assert!(
            !is_tool_error(&nav),
            "navigate to {shop} failed through the proxy:\n{}",
            text_of(&nav)
        );
        let snap = browser.call("browser_snapshot", json!({})).await;
        let text = text_of(&snap);
        let price = fixture.price(shop);
        assert!(
            text.contains(price),
            "{shop}: price {price} not in snapshot:\n{text}"
        );
        seen.push((shop, price.to_owned()));
    }
    // The fixture decides who is cheaper; the test only checks it is
    // derivable from what the browser saw, as the agent must do in F1.
    let numeric = |p: &str| -> u64 {
        p.chars()
            .filter(char::is_ascii_digit)
            .collect::<String>()
            .parse()
            .expect("price digits")
    };
    let cheapest = seen
        .iter()
        .min_by_key(|(_, p)| numeric(p))
        .map(|(s, _)| *s)
        .expect("two shops");
    assert_eq!(cheapest, fixture.cheaper(), "prices read: {seen:?}");

    // --- F2 (minimal): denied host is refused at the proxy ---
    let denied = format!("https://localhost:{}/", fixture.port("magpie"));
    let nav = browser
        .call("browser_navigate", json!({"url": denied}))
        .await;
    assert!(
        is_tool_error(&nav),
        "navigate to localhost (off the allowlist) must fail, got:\n{}",
        text_of(&nav)
    );

    browser.stop().await;
    fixture.stop().await;
}

/// D1: live mode must not start an unproxied Chromium. No `HTTPS_PROXY` →
/// the child exits non-zero before any MCP traffic. This one needs no
/// browser, so it is not gated.
#[tokio::test]
async fn live_mode_refuses_to_launch_without_the_proxy_env() {
    let mur_home = tempfile::tempdir().expect("tempdir");
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["browser", "record", "--run", "live-d1", "--mode", "live"])
        .env("MUR_HOME", mur_home.path())
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .stdin(Stdio::null())
        .output()
        .await
        .expect("run mur");
    assert!(!out.status.success(), "live mode launched without a proxy");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("egress proxy"),
        "expected the D1 refusal, got:\n{stderr}"
    );
}
