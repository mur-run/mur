use super::*;
use tokio::io::duplex;

/// A fake server: echoes `{"id":…,"result":{"echo":<params>}}` for every
/// request, and answers `tools/list` with one tool.
async fn fake_server(mut inp: impl AsyncRead + Unpin, mut out: impl AsyncWrite + Unpin) {
    let mut lines = BufReader::new(&mut inp).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let v: Value = serde_json::from_str(&line).unwrap();
        let id = v["id"].clone();
        let result = if v["method"] == "tools/list" {
            json!({"tools":[{"name":"browser_navigate"}]})
        } else {
            json!({"echo": v["params"]})
        };
        let resp = json!({"jsonrpc":"2.0","id":id,"result":result}).to_string();
        out.write_all(resp.as_bytes()).await.unwrap();
        out.write_all(b"\n").await.unwrap();
    }
}

/// Hook that rejects `browser_storage_state`, answers `mur_ping` itself,
/// and makes one internal call on `browser_click` to prove `Downstream`.
struct TestHook {
    internal_seen: Arc<Mutex<Vec<Value>>>,
}

#[derive(Clone)]
struct FakeBroker;

impl BrokerClient for FakeBroker {
    fn transform<'a>(
        &'a self,
        mut value: Value,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<crate::broker::Transform>> + Send + 'a>,
    > {
        Box::pin(async move {
            replace_test_secret(&mut value, "{{secret:pchome/PASSWORD}}", "ActualSecret123!");
            Ok(crate::broker::Transform {
                value,
                lease: "one-shot-test-lease".into(),
                boundary: Some(json!({"keys":["PASSWORD"]})),
            })
        })
    }

    fn redact<'a>(
        &'a self,
        lease: String,
        _boundary: Option<Value>,
        mut value: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Value>> + Send + 'a>>
    {
        Box::pin(async move {
            anyhow::ensure!(lease == "one-shot-test-lease", "unexpected broker lease");
            replace_test_secret(&mut value, "ActualSecret123!", "[redacted:PASSWORD]");
            Ok(value)
        })
    }
}

fn replace_test_secret(value: &mut Value, from: &str, to: &str) {
    match value {
        Value::String(text) => *text = text.replace(from, to),
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| replace_test_secret(value, from, to)),
        Value::Object(values) => values
            .values_mut()
            .for_each(|value| replace_test_secret(value, from, to)),
        _ => {}
    }
}

struct CaptureRequestHook {
    seen_response_request: Arc<Mutex<Option<Request>>>,
}

impl Hook for CaptureRequestHook {
    async fn on_request(&mut self, req: Request, _down: &Downstream) -> Decision {
        Decision::Forward(req)
    }

    async fn on_response(&mut self, req: &Request, resp: Value, _down: &Downstream) -> Value {
        *self.seen_response_request.lock().await = Some(req.clone());
        resp
    }
}
impl Hook for TestHook {
    async fn on_request(&mut self, req: Request, down: &Downstream) -> Decision {
        match req.tool_name() {
            Some("browser_storage_state") => Decision::Reject {
                code: -32000,
                message: "use mur browser auth".into(),
            },
            Some("mur_ping") => Decision::Reply(json!({"pong":true})),
            Some("browser_click") => {
                let r = down
                    .call_tool("browser_generate_locator", json!({"ref":"e1"}))
                    .await
                    .unwrap();
                self.internal_seen.lock().await.push(r);
                Decision::Forward(req)
            }
            _ => Decision::Forward(req),
        }
    }
    async fn on_response(&mut self, _req: &Request, mut resp: Value, _d: &Downstream) -> Value {
        resp["result"]["hooked"] = json!(true);
        resp
    }
    fn extra_tools(&self) -> Vec<Value> {
        vec![json!({"name":"mur_ping"})]
    }
}

async fn read_line(r: &mut (impl AsyncRead + Unpin)) -> Value {
    let mut lines = BufReader::new(r).lines();
    let l = tokio::time::timeout(std::time::Duration::from_secs(2), lines.next_line())
        .await
        .expect("timeout")
        .unwrap()
        .expect("eof");
    serde_json::from_str(&l).unwrap()
}

#[test]
fn storage_state_export_code_quotes_hostile_paths() {
    let path = std::path::Path::new("/private/a\"b\\c/storage-state.json");
    let code = storage_state_export_code(path).unwrap();
    assert!(code.starts_with("async (page) =>"));
    assert!(code.contains("storageState({ path: \"/private/a\\\"b\\\\c/storage-state.json\" })"));
    assert!(code.ends_with("return 'storage state saved'; }"));
}

#[test]
fn headed_auth_args_use_selected_browser_and_default_headed_mode() {
    let output_arg = "--output-dir=/private/tmp/mur-auth".to_owned();
    let browser_arg = "--browser=firefox".to_owned();
    let args = playwright_args(&["--isolated".into(), browser_arg.clone(), output_arg.clone()]);

    assert!(args.contains(&"--isolated".to_owned()));
    assert!(args.contains(&browser_arg));
    assert!(args.contains(&output_arg));
    assert!(
        !args.iter().any(|arg| arg == "--headed"),
        "@playwright/mcp is headed by default; --headed is not a supported option"
    );
}

/// The agent hanging up (stdin EOF) must end the session even while the
/// downstream server stays alive and silent, like a real Playwright MCP.
#[tokio::test]
async fn agent_eof_ends_session_while_server_stays_up() {
    let (agent_w, agent_in) = duplex(64 * 1024);
    let (agent_out, _agent_r) = duplex(64 * 1024);
    let (server_in, srv_r) = duplex(64 * 1024);
    let (srv_w, server_out) = duplex(64 * 1024);
    // The server only exits once its stdin closes.
    tokio::spawn(fake_server(srv_r, srv_w));
    let session = tokio::spawn(run_io(agent_in, agent_out, server_in, server_out, PassHook));
    drop(agent_w);
    tokio::time::timeout(std::time::Duration::from_secs(2), session)
        .await
        .expect("run_io did not return after the agent closed stdin")
        .unwrap()
        .unwrap();
}

/// When the server dies first, `run_io` returns while the agent's stdin is
/// still open. A read blocked on stdin must not keep the runtime from
/// shutting down, or `mur browser record` outlives its own server.
#[cfg(unix)]
#[test]
fn blocked_agent_read_does_not_hold_runtime_shutdown() {
    let (_agent_keeps_open, read_end) = std::os::unix::net::UnixStream::pair().unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            use tokio::io::AsyncReadExt;
            let mut r = detached_reader(read_end);
            let mut buf = [0u8; 1];
            let _ =
                tokio::time::timeout(std::time::Duration::from_millis(100), r.read(&mut buf)).await;
        });
        drop(rt);
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .expect("runtime shutdown blocked on a pending stdin read");
}

#[tokio::test]
async fn detached_reader_relays_bytes_then_eof() {
    use tokio::io::AsyncReadExt;
    let mut r = detached_reader(std::io::Cursor::new(b"{\"a\":1}\n".to_vec()));
    let mut got = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        r.read_to_string(&mut got),
    )
    .await
    .expect("no EOF after the source ended")
    .unwrap();
    assert_eq!(got, "{\"a\":1}\n");
}

struct PassHook;
impl Hook for PassHook {
    async fn on_request(&mut self, req: Request, _down: &Downstream) -> Decision {
        Decision::Forward(req)
    }
    async fn on_response(&mut self, _req: &Request, resp: Value, _down: &Downstream) -> Value {
        resp
    }
}

#[tokio::test]
async fn forward_reject_reply_and_internal_call() {
    let (agent_w, agent_in) = duplex(64 * 1024); // test writes → proxy stdin
    let (agent_out, mut agent_r) = duplex(64 * 1024); // proxy stdout → test reads
    let (server_in, srv_r) = duplex(64 * 1024); // proxy → server
    let (srv_w, server_out) = duplex(64 * 1024); // server → proxy

    tokio::spawn(fake_server(srv_r, srv_w));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let hook = TestHook {
        internal_seen: seen.clone(),
    };
    tokio::spawn(run_io(agent_in, agent_out, server_in, server_out, hook));

    let mut agent_w = agent_w;
    let send = |v: Value| v.to_string() + "\n";

    // 1. plain forward + on_response touched it
    agent_w
        .write_all(
            send(json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":"browser_navigate","arguments":{"url":"https://x"}}}))
            .as_bytes(),
        )
        .await
        .unwrap();
    let r = read_line(&mut agent_r).await;
    assert_eq!(r["id"], 1);
    assert_eq!(r["result"]["echo"]["name"], "browser_navigate");
    assert_eq!(r["result"]["hooked"], true);

    // 2. reject never reaches server
    agent_w
        .write_all(
            send(json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                "params":{"name":"browser_storage_state","arguments":{}}}))
            .as_bytes(),
        )
        .await
        .unwrap();
    let r = read_line(&mut agent_r).await;
    assert_eq!(r["id"], 2);
    assert_eq!(r["error"]["message"], "use mur browser auth");

    // 3. reply from hook
    agent_w
        .write_all(
            send(json!({"jsonrpc":"2.0","id":"s3","method":"tools/call",
                "params":{"name":"mur_ping","arguments":{}}}))
            .as_bytes(),
        )
        .await
        .unwrap();
    let r = read_line(&mut agent_r).await;
    assert_eq!(r["id"], "s3");
    assert_eq!(r["result"]["pong"], true);

    // 4. internal call happens before forward; agent sees only its own reply
    agent_w
        .write_all(
            send(json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
                "params":{"name":"browser_click","arguments":{"ref":"e1"}}}))
            .as_bytes(),
        )
        .await
        .unwrap();
    let r = read_line(&mut agent_r).await;
    assert_eq!(r["id"], 4);
    assert_eq!(r["result"]["echo"]["name"], "browser_click");
    let seen = seen.lock().await;
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0]["result"]["echo"]["name"],
        "browser_generate_locator"
    );
    assert!(seen[0]["id"].as_str().unwrap().starts_with("mur-"));

    // 5. tools/list gets extra tools merged
    agent_w
        .write_all(send(json!({"jsonrpc":"2.0","id":5,"method":"tools/list"})).as_bytes())
        .await
        .unwrap();
    let r = read_line(&mut agent_r).await;
    let names: Vec<_> = r["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["browser_navigate", "mur_ping"]);
}

#[tokio::test]
async fn broker_hook_forwards_secret_but_records_placeholder_and_redacts_response() {
    let (agent_w, agent_in) = duplex(64 * 1024);
    let (agent_out, mut agent_r) = duplex(64 * 1024);
    let (server_in, mut server_r) = duplex(64 * 1024);
    let (mut server_w, server_out) = duplex(64 * 1024);
    let seen = Arc::new(Mutex::new(None));
    let hook = BrokerHook::new(
        CaptureRequestHook {
            seen_response_request: seen.clone(),
        },
        Arc::new(FakeBroker),
    );
    tokio::spawn(run_io(agent_in, agent_out, server_in, server_out, hook));

    let server = tokio::spawn(async move {
        let request = read_line(&mut server_r).await;
        assert_eq!(request["params"]["arguments"]["text"], "ActualSecret123!");
        let response = json!({
            "jsonrpc":"2.0", "id":request["id"],
            "result":{"content":"echo ActualSecret123!"}
        });
        server_w
            .write_all(response.to_string().as_bytes())
            .await
            .unwrap();
        server_w.write_all(b"\n").await.unwrap();
    });

    let mut agent_w = agent_w;
    agent_w.write_all(
            br#"{"jsonrpc":"2.0","id":77,"method":"tools/call","params":{"name":"browser_type","arguments":{"text":"{{secret:pchome/PASSWORD}}"}}}
"#,
        ).await.unwrap();
    let response = read_line(&mut agent_r).await;
    server.await.unwrap();

    assert_eq!(response["result"]["content"], "echo [redacted:PASSWORD]");
    assert!(!response.to_string().contains("ActualSecret123!"));
    let recorded = seen.lock().await.clone().expect("response hook request");
    assert_eq!(
        recorded.tool_args().unwrap()["text"],
        "{{secret:pchome/PASSWORD}}"
    );
}

#[cfg(unix)]
#[test]
fn private_output_dir_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let path = private_output_dir().unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o700
    );
    std::fs::remove_dir(path).unwrap();
}

#[test]
fn request_accessors() {
    let mut r: Request = serde_json::from_value(json!({"jsonrpc":"2.0","id":1,
            "method":"tools/call","params":{"name":"browser_type","arguments":{"text":"a"}}}))
    .unwrap();
    assert_eq!(r.tool_name(), Some("browser_type"));
    assert_eq!(r.tool_args().unwrap()["text"], "a");
    r.tool_args_mut().unwrap()["text"] = json!("b");
    assert_eq!(r.tool_args().unwrap()["text"], "b");
    let n: Request =
        serde_json::from_value(json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .unwrap();
    assert!(n.tool_name().is_none());
    assert!(n.id.is_none());
}
