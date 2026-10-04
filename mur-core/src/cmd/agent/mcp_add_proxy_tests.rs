//! #1639: the install-time probe must spawn a `Restricted` server the way the
//! runtime does — with a live egress proxy and a tokened `HTTPS_PROXY` — or a
//! server that (correctly) refuses to start unproxied can never be installed.
//! #1647 extends the same rule to `mur agent mcp inspect`, which shares the
//! probe helper (`agent_mcp_pin::probe_as_runtime_would`).
//!
//! The fake server below mirrors the live-mode rule in `cmd/browser`: no
//! tokened proxy URL ⇒ exit 1 before speaking MCP. It is written into a
//! tempdir at test time (no repo-layout dependency) and needs only `/bin/sh`.
//!
//! Running these from INSIDE a MUR agent seal: the seal denies exec from the
//! system temp dir, so the fake server fails with `Operation not permitted`
//! before any assertion is reached. Point the tempdir at an exec-able path:
//! `TMPDIR=<repo>/target/exec-tmp cargo test -p mur-core --lib proxy_tests`.
//! Only for these tests — a long `TMPDIR` pushes Unix-socket paths in other
//! suites past the 104-byte `sun_path` limit. CI is unaffected.

use super::*;
use mur_common::agent::{McpNetMode, McpServerEntry, McpServerNetwork};

const FAKE_SERVER: &str = r#"#!/bin/sh
rec="$1"; mode="$2"
case "$HTTPS_PROXY" in
  http://?*:x@127.0.0.1:*) ;;
  *) echo "fake-mcp: HTTPS_PROXY missing or tokenless: '${HTTPS_PROXY}'" >&2; exit 1 ;;
esac
printf '%s\n' "$HTTPS_PROXY" > "$rec"
if [ "$mode" = fail ]; then
  echo "fake-mcp: failing on purpose after recording the proxy" >&2
  exit 1
fi
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  [ -z "$id" ] && continue
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"0"}}}\n' "$id" ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"ping","description":"fake","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *)
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id" ;;
  esac
done
"#;

struct Fixture {
    _tmp: tempfile::TempDir,
    _env: mur_common::test_env::EnvGuard,
    script: std::path::PathBuf,
    record: std::path::PathBuf,
    profile: mur_common::AgentProfile,
}

fn fixture(mode: &str) -> Fixture {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let script = work.join("fake-mcp.sh");
    std::fs::write(&script, FAKE_SERVER).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let record = work.join("https_proxy.txt");

    let mur_home = tmp.path().join("mur-home");
    std::fs::create_dir_all(mur_home.join("agents").join("carol")).unwrap();
    let mut env = mur_common::test_env::EnvGuard::hold();
    env.set_var("MUR_HOME", &mur_home);
    // A broken handshake must fail the test, not hang it.
    env.set_var("MUR_MCP_PROBE_TIMEOUT_S", "10");

    let mut profile = mur_common::AgentProfile::default_for_tests();
    let w = work.display().to_string();
    profile.entitlements.filesystem.read.push(w.clone());
    profile.entitlements.filesystem.write.push(w);
    profile.mcp_servers.push(McpServerEntry {
        name: "live-srv".into(),
        command: script.display().to_string(),
        args: vec![record.display().to_string(), mode.into()],
        network: Some(McpServerNetwork {
            mode: McpNetMode::Restricted,
            allow_hosts: vec!["example.com".into()],
            ..Default::default()
        }),
        ..Default::default()
    });
    Fixture {
        _tmp: tmp,
        _env: env,
        script,
        record,
        profile,
    }
}

/// Parse the port the child was told to dial, from what it actually received.
fn recorded_proxy_port(record: &std::path::Path) -> u16 {
    let url = std::fs::read_to_string(record)
        .unwrap_or_else(|e| panic!("fake server never recorded HTTPS_PROXY ({e})"));
    url.trim()
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("no port in recorded proxy url {url:?}"))
}

fn assert_port_closed(port: u16) {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let r = std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500));
    assert!(
        r.is_err(),
        "probe's egress proxy on {addr} still accepts connections after the probe returned \
         — it leaked onto a runtime that outlived the probe"
    );
}

/// From an already-sealed shell (a MUR agent session) macOS refuses the
/// probe child's own seal with `EPERM`, so these tests cannot run (#1697).
/// Returns true after printing a visible SKIP line.
fn skip_if_nested_seal(test: &str) -> bool {
    let sealed = mur_agent_runtime::sandbox::current_process_sealed();
    if sealed {
        eprintln!(
            "SKIP {test}: this process is already sandboxed, so the probe's \
             restricted child cannot apply its own seal (#1697)"
        );
    }
    sealed
}

#[test]
fn probe_hands_a_restricted_server_a_tokened_proxy() {
    if skip_if_nested_seal("probe_hands_a_restricted_server_a_tokened_proxy") {
        return;
    }
    let f = fixture("ok");
    let res = probe_new_entry("carol", &f.profile, "live-srv", &f.script);
    let (_hash, tools) =
        res.unwrap_or_else(|e| panic!("restricted server must pass the probe: {e:#}"));
    assert_eq!(tools, 1);
    // Success path: the proxy the probe started must be gone with the probe.
    assert_port_closed(recorded_proxy_port(&f.record));
}

#[test]
fn probe_failure_still_tears_down_its_proxy() {
    if skip_if_nested_seal("probe_failure_still_tears_down_its_proxy") {
        return;
    }
    let f = fixture("fail");
    let err = probe_new_entry("carol", &f.profile, "live-srv", &f.script)
        .expect_err("a server that exits must fail the probe");
    // It must fail for OUR reason (it got the proxy, then quit) — not the
    // tokenless-proxy refusal this file exists to fix.
    let msg = format!("{err:#}");
    assert!(
        !msg.contains("tokenless"),
        "failed for the wrong reason: {msg}"
    );
    assert_port_closed(recorded_proxy_port(&f.record));
}

#[test]
fn inspect_probes_a_restricted_server_behind_a_tokened_proxy() {
    if skip_if_nested_seal("inspect_probes_a_restricted_server_behind_a_tokened_proxy") {
        return;
    }
    use crate::cmd::agent_mcp_pin::{InspectStatus, compute_binary_sha256, inspect_one_probed};
    let f = fixture("ok");
    let mut entry = f.profile.mcp_servers.last().unwrap().clone();
    // Real binary pin, so the binary side is CLEAN and only the probe decides.
    entry.binary_sha256 = Some(compute_binary_sha256(&f.script).unwrap());
    // A mismatched pin: reaching DESCRIPTION DRIFT proves `tools/list` was
    // answered, which the fake server only does when handed a tokened proxy.
    entry.description_hash = Some("0".repeat(64));
    let status = inspect_one_probed(
        "carol",
        &entry,
        std::time::Duration::from_secs(10),
        &mur_agent_runtime::sandbox::policy::SandboxPolicy::default(),
    );
    assert_eq!(
        status,
        InspectStatus::DescriptionDrift,
        "the probe must reach tools/list; StartupWouldFail means it ran unproxied"
    );
    assert_port_closed(recorded_proxy_port(&f.record));
}
