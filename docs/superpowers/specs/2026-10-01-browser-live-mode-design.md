# Browser Live Mode (Price Comparison) — Design

Date: 2026-10-01
Status: draft, pending review. Nothing here is implemented. Every claim is
tagged with how it is known:

- **[read]** read from source; not compiled or run
- **[probed]** observed in a probe run, log line quoted
- **[untested]** design intent only

## Main line

The goal is **live price comparison**: an agent opens a browser, searches,
reads prices from more than one shop, and reports them. The security boundary
below exists so that live mode can ship at all. It supports the main line and
does not replace it. When a security question and the main line conflict, the
first thing to verify is still "does comparison run", not "is every
cross-agent vector closed".

v1 guarantees three things, **under premise P1**:

| # | Guarantee |
|---|---|
| G1 | The browser reaches only hosts on the run's allowlist; everything else is refused at the proxy. |
| G2 | Every request the browser makes goes through the proxy: sub-resources, WebSocket, requests after CDP detach, and loopback. |
| G3 | Actions that need approval park on an approval card and run only after approval. |

**P1: no other same-uid process actively interferes.** The proxy is a port on
`127.0.0.1`. A same-uid process can kill it and bind the same port, and then
Chromium tunnels through the attacker's proxy. G1 and G2 do not hold against
that. Active cross-agent interference (signals, port stealing) is phase 2
(see the table at the end). v1 does not claim it.

## What exists today

The point of this section is to stop anyone reading the rest as "everything
is built except signal handling".

| Piece | State | Evidence |
|---|---|---|
| Live mode | **absent** | [read] `mur-browser/src/recorder.rs:170-173`: `pub enum Mode { Test, Automation }` |
| Chromium launched through a proxy | **absent** | [read] no `proxy.server` / `proxy_server` in `mur-browser/src` |
| `EffectKey` / approval-card key | **absent, and the design is not on disk** | [read] 0 hits for `EffectKey` in the repo, `docs/`, `openspec/` |
| Loopback egress proxy | exists, **advisory**, CONNECT-only | [read] `mur-agent-runtime/src/sandbox/egress_proxy.rs:1-5`, `:116` |
| Playwright MCP proxy flags | exist | [read] `@playwright/mcp` 0.0.82 README: `--proxy-server`, `--proxy-bypass` |
| launchd socket activation | **absent** | [read] 0 hits for `launch_activate_socket` / `Sockets` / `SockServiceName` |
| HITL gate (defer, match on `action_hash`) | exists | CLAUDE.md, `mur fleet` safety triad |

## Decisions

### D1 — The allowlist is enforced at the proxy, never through CDP

[probed] CDP sees a WebSocket but cannot pause it:

```
CDP saw websocket (Network only) ws://ex-ws.test/s
```

[probed] The proxy sees that WebSocket, and also a request issued after CDP
detached:

```
CONNECT ex-ws.test:80 HTTP/1.1
GET http://ex-late.test/after-detach HTTP/1.1
```

So CDP is an observation channel, not a boundary. Request interception over
CDP may be used for UX (showing what was blocked). It is never the reason a
request fails.

### D2 — The allowlist is the browser entry's `network` policy, with no built-in sites

The allowlist is the `network:` allowlist of the agent profile's `mur-browser`
MCP entry — the same policy `mcp_client.rs:467` already registers with the
proxy when it spawns the child. MUR ships no site list (CLAUDE.md rules 1
and 2). An empty allowlist denies everything. Real shops are the user's
input, not the product's.

**Revised 2026-10-01 (was: per-run input).** `register_policy` is an
in-process `Mutex<HashMap>` in the runtime (`egress_proxy.rs:60-70`);
`mur-browser` is a separate process and its crate does not — and per the
layering rule must not — depend on `mur-agent-runtime`. It therefore cannot
mint a per-run token. The only credential it can reach is the entry token
the runtime places in `HTTP_PROXY`'s userinfo. Live mode reuses that token,
so the allowlist is per-entry, not per-run. A per-run token would need the
proxy to expose a control endpoint (mint a sub-token under the entry
token); that is phase 2, not v1. Changing the allowlist means editing the
profile and restarting the agent, which is the existing `mur agent mcp`
contract.

### D3 — v1 acceptance runs on local fixtures, not real shops

A real shop mixes four failure layers: allowlist too narrow (CDN hosts), bot
detection, login walls, and page structure. A failure on a real shop cannot
be attributed to one layer. v1 has to prove the security model and the
comparison loop on pages where each failure has one cause. Real shops are a
phase-2 manual smoke test (see below).

### D4 — v1 adds no anti-detection launch flags

Flags such as `--disable-blink-features=AutomationControlled` change the
browser's fingerprint and belong to a separate decision about how honestly
MUR presents itself to sites. v1 does not add them.

### D5 — Playwright MCP launches Chromium; MUR does not

Chromium is started by `@playwright/mcp`, as today (`playwright_command`,
`mur-browser/src/proxy.rs`). MUR does not launch Chromium itself and does not
hand it over with `--cdp-endpoint`. This keeps the council decision
(council-follow, architect, 2026-09-30): taking over Chromium's lifecycle
needs a launcher MUR does not have, and CDP `Fetch` may conflict with
Playwright's own routing.

The condition for keeping it was that Gap 0 can be closed without a MUR
launcher. [probed] It can (Gap 0, case D). Revisit only when a need cannot be
met at the proxy, such as pausing a request before approval or rules by
resource type or HTTP method.

Chromium runs with `--no-sandbox` (user direction, 2026-10-01). [probed]
Inside the MUR seal, Chromium's own sandbox cannot start
(`sandbox_extension_issue_file_to_process failed … Operation not permitted`).
Without the flag, the browser died at launch with
`Target page, context or browser has been closed`.

### D6 — The proxy identifies a run by its token, not by its port

The proxy keeps one listener per runtime (`egress_proxy.rs:82`, bound once at
seal time) and one registry entry per MCP child (`mcp_client.rs:467`); the
token in `Proxy-Authorization` selects the entry. Live mode keeps that. It
does not add a listener per run.

Per-port identity was the alternative: it would let the browser skip the
credential dance (Gap 0 case B) and drop Gaps 1 and 2. It was rejected on
the port-stealing probe (2026-10-01, run
`~/.mur/artifacts/mur/port-steal-probe/`, inside the MUR concierge's seal):

| Condition | Attacker `bind` on the proxy's port | Client dialing the old port saw |
|---|---|---|
| Listener alive | `EADDRINUSE` under all four `SO_REUSEADDR`/`SO_REUSEPORT` combinations | — |
| Listener closed | Bound, and `accept` got the client's `CONNECT` | `HTTP/1.1 200 STOLEN` |

[probed] A live listener cannot be hijacked by a same-uid process. A freed
port can be, and `bind`/`listen` on loopback is not denied by the seal (the
SBPL only has `network-outbound` rules, `macos.rs:323-371`). Ephemeral ports
are handed out monotonically (20 samples, 54420→54439), so the window is
small but real.

Consequences:

- A port alone proves who is listening only while the listener is up. A
  token also proves who is *sending*: an attacker that takes the port still
  needs the entry's token to be granted that entry's allowlist. The token
  is per-entry, not per-run (D2, revised): it lives as long as the
  `mur-browser` child does and is never written anywhere but the 0600
  config file (Gap 2). Token identity is still the stronger claim, so Gaps
  1 and 2 stay in v1.
- The proxy binding once per runtime, not per run, means there is no
  per-run port to steal at run end; the port outlives every run and dies
  with the runtime.
- What the token does **not** cover is unchanged from P1: a same-uid process
  that kills the runtime and rebinds its port sees the token in the next
  `CONNECT`. That is phase 2 (signal deny, port stealing), as before.

## Integration gaps (must be closed in v1)

Found by reading code for this spec. None has been run end-to-end.

### Gap 0 — The existing proxy wiring never reaches Chromium

[read] MCP children are pointed at the proxy only through environment
variables, `mur-agent-runtime/src/protocol/mcp_client.rs:471-476`:

```rust
("HTTP_PROXY".into(), url.clone()),
("HTTPS_PROXY".into(), url.clone()),
("http_proxy".into(), url.clone()),
("https_proxy".into(), url),
("NO_PROXY".into(), no_proxy.clone()),
("no_proxy".into(), no_proxy),
```

[read] `playwright-core@1.64.0-alpha` `lib/coreBundle.js` has 0 occurrences of
`HTTP_PROXY` / `HTTPS_PROXY`. Its proxy input is `PLAYWRIGHT_MCP_PROXY_SERVER`
(`:73917`) or the `--proxy-server` flag. So a `browser` MCP entry registered
as `Restricted` or `BroadAudited` today gets env vars that nothing in
Playwright reads.

[probed] Four wirings, each against a fake proxy on `127.0.0.1` that logs the
request line and `Proxy-Authorization` and never tunnels. Target
`https://probe-gap0.test/`, token `tok123`:

| Case | Proxy input | Fake proxy answers | Proxy saw | Navigation |
|---|---|---|---|---|
| A | today's `HTTP_PROXY`/`HTTPS_PROXY` env, token in URL | 403 | nothing | went direct (`ERR_CERT_COMMON_NAME_INVALID` from a local listener) |
| B | `PLAYWRIGHT_MCP_PROXY_SERVER=http://tok123:x@127.0.0.1:<port>` | 403 | every request; `Proxy-Authorization` never sent | `ERR_TUNNEL_CONNECTION_FAILED` |
| C | same as B | 407 challenge | every request; `Proxy-Authorization` never sent | `ERR_INVALID_AUTH_CREDENTIALS` |
| D | `--config` file: `proxy.server` + `username`/`password` | 407 challenge | first CONNECT bare, retry with `Proxy-Authorization: Basic dG9rMTIzOng=` (`tok123:x`) | `ERR_TUNNEL_CONNECTION_FAILED` (fake proxy never tunnels) |

- Chromium launched by Playwright MCP does not honour `HTTP_PROXY` (A).
- Renaming the env var is not the fix. It routes traffic to the proxy but
  loses the token (B, C). [read] `normalizeProxySettings` rebuilds `server`
  as `protocol + "//" + host` (`coreBundle.js:52274`), dropping the userinfo.
- **The fix is Gap 2's config file together with Gap 1's 407.**
  Neither alone is enough (C has 407 without config; B has neither).
- Per-port identity (B alone, no credentials) was considered and rejected;
  see D6.

Probe setup: `@playwright/mcp@0.0.82`, `playwright-core@1.64.0-alpha`, full
Chrome for Testing r1246 via `--executable-path`, `--headless --isolated
--no-sandbox`. It ran inside the MUR concierge's seal, not the live-mode seal.
Two seal findings to recheck under the live-mode seal: exec of
`chrome-headless-shell` was denied (`Operation not permitted`), and Playwright
MCP writes `~/Library/Caches/ms-playwright/b` (`serverRegistry`,
`coreBundle.js:53178`), so the probe pointed `HOME` at a writable directory.

[probed 2026-10-02] Both findings are profile grants, not seal limits.
`mur-core/tests/browser_live_f1.rs` passes inside the MUR concierge's seal
(2 passed, 8.45 s; both fixture shops logged `GET /`) once the agent profile
holds all three of:

| Requirement | Failure without it |
|---|---|
| Spawn grant on the **full path** of `chrome-headless-shell` (under `~/Library/Caches/ms-playwright/chromium_headless_shell-<rev>/…`); the bare name is not enough | `spawn EPERM` |
| Write grant on `~/Library/Caches/ms-playwright/b` (one `browser@<hash>` file per launch) | `EPERM: open …/ms-playwright/b/browser@<hash>` in `initializeServer` |
| `MUR_BROWSER_CHROMIUM_ARGS=--no-sandbox` (D5) | Chromium dies at launch |

Grant the `b/` directory only, not all of `ms-playwright`. The spawn grant is
pinned to a Chromium revision, so a Playwright upgrade that changes `<rev>`
needs a new grant. Each run logs one
`WARN mur_browser::recorder: action failed downstream; step not recorded
tool="browser_navigate"`. This is expected: it is the minimal F2 check in
`mur-core/tests/browser_live_f1.rs:291-299`, which navigates to an
off-allowlist host and asserts the proxy refuses it; the recorder skips the
failed step. Five consecutive runs on 2026-10-02 (120 s cap each) all passed
in 7–9 s with exactly one such WARN. One earlier run hung for >600 s with no
captured output; cause unknown, not reproduced.

### Gap 1 — The proxy answers 403 where Chromium needs 407

[read] The proxy identifies a client by the token in `Proxy-Authorization:
Basic …` (`egress_proxy.rs:126-128`). A missing token is treated as unknown
and gets `HTTP/1.1 403 Forbidden` (`:137`, `:146`).

[read] Playwright supplies proxy credentials on Chromium only in answer to an
auth challenge. `authenticateProxyViaCredentials` (`coreBundle.js:52622-52628`)
stores them as `httpCredentials`. The `Fetch.authRequired` handler
(`:36895-36906`) replies `ProvideCredentials`. A 403 is not a challenge, so no
credentials are ever sent, and **every browser CONNECT would be denied**.
[probed] Confirmed by the Gap 0 probe: with credentials configured (case D),
Chromium sends the first CONNECT bare and adds `Proxy-Authorization` only
after a 407 challenge.

Fix: when the request carries no `Proxy-Authorization` header, reply
`407 Proxy Authentication Required` with `Proxy-Authenticate: Basic
realm="mur"`. Keep 403 for a present-but-unknown token and for a disallowed
host. Existing MCP children send the header up front, so their behaviour does
not change.

### Gap 2 — Proxy credentials cannot be passed on the command line

[read] The `--proxy-server` CLI path builds `{ server, bypass }` only
(`coreBundle.js:73804-73809`). Credentials exist only as config-file keys
`browser.launchOptions.proxy.username` / `.password` (`:73569-73570`), and the
config file is chosen with `--config` / `PLAYWRIGHT_MCP_CONFIG` (`:73890`).

Fix: `mur-browser` parses the proxy URL the runtime already exports in
`HTTPS_PROXY` (`http://<token>:x@127.0.0.1:<port>`, `mcp_client.rs:467-472`),
writes an MCP config file with mode 0600 in a 0700 private directory
(`{"browser":{"launchOptions":{"proxy":{"server","username","password"}}}}`),
passes it with `--config`, and deletes it when the Playwright child exits.
[read] `@playwright/mcp` reads the file once at process start
(`resolveCLIConfigForMCP` → `loadConfig`, `coreBundle.js:73641-73645`), so
there is no earlier moment worth hooking. Implemented as `mur browser record
--mode live` (`mur-core/src/cmd/browser/mod.rs`, `live_config_for`) over
`mur-browser/src/live_proxy.rs`; the same `record` entry the browser MCP
entry already launches, so no new launcher. The
token is the entry token (D2, revised); the file dies with the launch. If
`HTTPS_PROXY` is absent or carries no userinfo, live mode refuses to launch
rather than starting an unproxied Chromium (D1).

### Gap 3 — The proxy forwards CONNECT only

[read] `egress_proxy.rs:116`: `// MVP supports CONNECT (https) only; plain
http forwarding is a follow-up.` Anything else gets `501 Not Implemented`
(`:121-123`). Through a proxy, Chromium sends plain-HTTP requests as `GET
http://…`, as the probe log shows. That fails closed, so it is not a hole, but
`http://` pages and plain-HTTP fixtures break.

**Open decision:** (a) add plain-HTTP forwarding under the same allowlist, or
(b) keep 501 and serve fixtures over HTTPS. Recommendation: (a), because real
sites still issue some `http://` sub-requests, and 501 would show up as an
unexplained breakage rather than a DENY line.

### Gap 4 — WebRTC UDP does not use the HTTP proxy

[untested] Chromium sends WebRTC/STUN over UDP, and an HTTP proxy does not
carry UDP. The OS seal filters by port, not host (`egress_proxy.rs:1-5`), so
UDP is not covered by G2 unless the launch disables non-proxied UDP.
Candidate switch: `--force-webrtc-ip-handling-policy=disable_non_proxied_udp`.
It must be verified by acceptance test F5 and must not be assumed.

### Not a gap: loopback is proxied

[read] Playwright forces loopback through the proxy unless the bypass list
names loopback: `shouldProxyLoopback` (`coreBundle.js:39043`), with the MCP
path prepending `<-loopback>` (`:39125`). So fixtures on `127.0.0.1` do
exercise the proxy. v1 must not pass a bypass list that re-enables the
loopback bypass.

### Background connections

[probed] The egress probe's proxy log showed Chrome's own traffic, for example
`CONNECT accounts.google.com:443` and `CONNECT android.clients.google.com:443`.
The probe's launch command was not saved in the probe directory, so it is not
known whether that Chrome ran with Playwright's default switches. [read]
Playwright adds `--disable-background-networking` (`coreBundle.js:35580`),
`--disable-component-update` (`:35588`) and `--disable-field-trial-config`
(`:35578`). [probed] They appear anyway: under a Playwright MCP launch (Gap 0
probe, cases B–D) the proxy saw `GET http://clients2.google.com/time/1/current…`
(plain HTTP, see Gap 3), `CONNECT update.googleapis.com:443`,
`CONNECT www.google.com:443` and `CONNECT accounts.google.com:443` before and
around the navigation. Test F7 counts them. They are denied, and F1 must still
pass.

## Architecture (v1)

```
agent ──MCP──▶ mur browser record --mode live   (mur-browser, Mode::Live)
                 │  token = userinfo of HTTPS_PROXY (set by the runtime,
                 │          mcp_client.rs:467, from the entry's allowlist)
                 │  writes 0600 MCP config {proxy.server, username=token}
                 ▼
              @playwright/mcp --config <run cfg> --no-sandbox ──▶ Chromium
                                                        │ all HTTP(S)/WS
                                                        ▼
                                       egress proxy 127.0.0.1:<ephemeral>
                                       407 → token → allowlist → tunnel / 403
```

- `Mode::Live` is added next to `Test` and `Automation`. Live runs are
  interactive: they are not replayed and heal does not apply to them.
- The proxy is the existing `egress_proxy`, extended with Gaps 1 and 3. There
  is no second proxy implementation.
- Approval card (G3): the existing chat gate with live-mode tool rules and a
  per-call effect key. See "Approval card (G3)" below.

## Approval card (G3)

The card is the existing chat gate, not a new one. What is decided here is
which `mur-browser` tools reach it and what key identifies one approved
effect.

### What the gate already does

[read] A tool with no rule defaults to `ToolPolicy::Ask` (`agent.rs:929`).
Rules match the wire name `mcp__<server>__<tool>` exactly or by `prefix*`
(`agent.rs:955-967`). An `Ask` call is refused outright when the seal is not
enforcing (`guarded.rs:117-121`, `refuse_unsandboxed`); otherwise it is
parked and the runtime waits (`batch.rs:39-60`). Before parking, the gate
looks the call's `action_hash` up in the decision store and, inside
`APPROVAL_TTL_SECS` (7 days, `mur-common/src/hitl/mod.rs:14`), replays an earlier allow or
deny without asking (`batch.rs:43-53`, "approved earlier"). The hash is
`chat_action_hash(tool, input, agent)` (`batch.rs:158`), which fixes the
channel slot to `""` and the step slot to `"chat"` (`store.rs:23`) so that
"same tool, same input" is one decision.

### Which browser actions need approval

The proxy already answers "where may the browser go" (G1, G2). The card only
has to answer "may the browser touch things outside the page". Classified
from Playwright MCP's own tool `type` tag (`coreBundle.js`, 0.0.82):

| Policy | Tools | Why |
|---|---|---|
| Allow | `type: readOnly`, `input`, `assertion`, and the `action` navigation set (`navigate`, `navigate_back`, `navigate_forward`, `reload`, `resize`, `close`, `handle_dialog`, `wait_for`, `snapshot`, `find`, `tabs`, `console_messages`, `network_requests`, `emulate_media`, `drag`, `drop`, `mouse_*`, `fill_form`) | Stays inside the page. Egress is bounded by the allowlist at the proxy. A price comparison needs nothing else. |
| Ask | `file_upload`, `evaluate`, `pdf_save`, `take_screenshot` (writes a file), `start_video`, `start_tracing`, `start_recording`, `set_storage_state`, `storage_state`, `cookie_set`, `cookie_*` mutations, `localstorage_set`, `sessionstorage_set`, `route`, `unroute`, `network_state_set` | Reads or writes the host filesystem, runs page-scoped code, or imports/exports credentials. |
| Deny | `run_code_unsafe`, `network_request`, `webmcp_call` | `run_code_unsafe` runs Node-side code outside the page. `network_request` is issued by Playwright's request context, not the browser; [untested] whether it honours the context proxy. Denied until probed. `webmcp_call` is out of scope. |

The rules are shipped as `ToolRule`s on the live-mode server entry under the
`mcp__mur-browser__` prefix; the agent profile can only tighten them (Allow →
Ask, Ask → Deny), never loosen. Wildcards are per-family (for example
`mcp__mur-browser__browser_cookie_*` → Ask, then exact-match `cookie_get`,
`cookie_list` → Allow, which wins because exact beats prefix).

### The effect key

An approved effect is one call, not one shape of call. For a live run the
key is

```
EffectKey = action_hash(tool, input, channel = <live run id>, step = <call id>, agent)
```

using the existing pin (`pin.rs:52-58`), not a new hash. Compared with the
chat key:

- The channel slot is the run id, so a decision never leaks across runs.
- The step slot is the call id, so a decision never leaks across calls.
  Two identical `file_upload` calls in one run are two cards.

This is the property F6 tests: **deny means not executed; approve means
executed exactly once.** The chat key cannot give it, because its 7-day
memory would replay the first approval onto every later identical call.

[read] Code change needed: `hitl::batch::pending` (`batch.rs:155-163`)
spells `chat_action_hash` itself. It needs a variant that takes the channel
and step slots, and the live-mode `ToolCallResult` must carry the run id.
Nothing else changes: the store, the responder's hash check
(`channel.rs:111-126`), and the defer-never-time-out rule are untouched.
The responder echoes the request's hash, so it does not recompute it and
does not need the run id.

### What the card is not

- It is not a network gate. A `navigate` to a host off the allowlist is
  refused at the proxy with no card (D1). A card that could pause a request
  in flight is the case D5 names for revisiting.
- It is not resumable across a runtime restart. Deferred cards survive
  (existing HITL), but a live run does not: the Chromium profile is
  `--isolated` and the config file dies with the launch (Gap 2), so a late approval
  resolves to a run that no longer exists and is dropped.

## Acceptance (v1)

All tests run against local fixtures: two "shops" served on `127.0.0.1` at
ephemeral ports, each with a product page that has a known price. The
allowlist is `127.0.0.1`. The denied host is `localhost`. [read] The proxy
matches the CONNECT host string (`egress_proxy.rs:136`, `host_allowed(host,
&e.allow)`), so `localhost` is denied by name even though it resolves to the
same machine. [untested] This must be confirmed in F2 before the other tests
rely on it.

| # | Test | Pass condition |
|---|---|---|
| F1 | **Comparison runs** | Agent opens both shops, reads both prices, reports both and names the cheaper one. |
| F2 | Denied sub-resources | Fixture page loads `img`, `fetch`, `sendBeacon`, and a WebSocket against `localhost:<port>`. Each shows as `egress proxy CONNECT DENY` (`egress_proxy.rs:144`), and the fixture server logs no hit. |
| F3 | After CDP detach | A request fired after CDP detaches (as in the probe) is denied at the proxy. |
| F4 | Proxy auth | Raw CONNECT without a header returns 407. With an unknown token it returns 403. With a valid token and an allowed host it returns 200. |
| F5 | WebRTC | Page opens `RTCPeerConnection` with a STUN server on a local UDP port. The listener receives zero packets. |
| F6 | Approval card | Two identical `browser_file_upload` calls in one run raise two cards. Deny: the fixture never receives the upload. Approve: it receives it exactly once. A `browser_run_code_unsafe` call is refused with no card. |
| F7 | Background traffic | Informational, not pass/fail: log the count and hosts of non-fixture CONNECTs. |

**Where they run.** Chromium-level tests need a real browser. In an
ungranted seal, `chrome-headless-shell --dump-dom` returned exit
0 with empty output even for a `data:` URL, so nothing about Chromium's
network behaviour could be observed from there. With the three grants listed
under Gap 0, the tool-level F1 runs inside the seal too. F1–F7 are opt-in end-to-end
tests gated by `MUR_BROWSER_E2E=1`, run in CI or a plain terminal. The
fixture is `scripts/e2e/browser-live-fixture.py` (two HTTPS shops on
`127.0.0.1`, self-signed cert minted per start — HTTPS because of Gap 3);
the tool-level F1 + minimal F2 is `mur-core/tests/browser_live_f1.rs`; the
LLM-in-the-loop F1 is `scripts/e2e/browser-live-f1.sh`.
F4 is a pure proxy test and runs in the normal suite, next to the existing
tests in `egress_proxy.rs` (`bare_connect_is_challenged_with_407`; the
unknown-token 403 and valid-token 200 cases were already covered by
`allowed_host_tunnels_denied_host_403`). F4 only proves the proxy *issues*
the challenge. That Chromium *answers* it with the config-file credentials
is the D row of the matrix above and is integration-only: it needs Gap 0
(proxy wired into the launch) and Gap 2 (the config file) in place,
so it lands with F1–F7, not in the unit suite.

## Phase 2 — conditional

### Signal deny

Filled in by the signal probe, which must run outside any seal. The probe's
cases are 1a (does `deny signal (target others)` block a real signal), 1c
(does it also gate signal 0), 2 (is `same-sandbox` per instance), 3a (does
`others` include one's own descendants), and 3b (a same-sandbox shell
signalling its own grandchild).

| Probe result | Signal handling |
|---|---|
| 1a DENIED + 3a ALLOWED | Handled: add the deny only. |
| 1a DENIED + 3a DENIED + 3b ALLOWED + 2 DENIED | Handled: deny, plus allow `same-sandbox`. |
| 1a DENIED + 3a DENIED + 2 ALLOWED | This route cannot work. Choose another mechanism or do not do it. |
| 1a ALLOWED | Accepted risk: the deny does not block signals in practice. |
| 1c DENIED | Fix both `pid_alive` implementations in the same PR: `mur-common/src/lock_file.rs:72` and `mur-agent-runtime/src/tools/bash_jobs.rs:423`. (`mur-core/src/cmd/agent/mod.rs:230` delegates to the first.) |

1c DENIED is documented behaviour, not a bug. `mur-common/src/lock_file.rs:61`
defines the result as "a live process the calling user can signal". The row
exists because a sealed caller would then read live processes as dead.

### Port stealing

**Not implemented.** [read] launchd socket activation has 0 hits in code.
[probed] The launchd probe directory contains only `probe.plist`, and no
`out.txt` was produced. [probed] The takeover itself is confirmed (D6 table):
a live proxy port cannot be taken by a same-uid process, a freed one can, and
the seal does not deny loopback `bind`/`listen`. The remaining phase-2 work is
the defence, not the demonstration: either keep the listener alive for the
runtime's whole life (already the case, one listener per runtime) and detect
the runtime being killed, or move to a socket that carries a peer identity
(launchd activation or a unix socket with `LOCAL_PEERCRED`).

### Real shops (manual smoke)

Real sites are exercised by hand, with the allowlist supplied by the user.
Each failure is classified by layer before anything is changed:

| Symptom | Layer | Next step |
|---|---|---|
| DENY lines for CDN / image hosts | Allowlist too narrow | User adds the hosts to their run's allowlist. |
| Page served but is a challenge or block page | Bot detection | Separate decision (see D4). |
| Price hidden behind sign-in | Login | `mur browser auth` storage state; out of v1. |
| Page loads, price not found | Page structure | Agent / prompt issue, not security. |

## Open items

1. ~~The G3 approval-card section.~~ Written. Remaining: probe whether
   `browser_network_request` honours the context proxy (it is Deny until
   then).
2. Gap 3 decision: plain-HTTP forwarding (recommended) or HTTPS-only fixtures.
3. Signal probe run outside the seal. This fills the phase-2 table and blocks
   nothing in v1.
4. ~~D6: token or per-agent port as the proxy identity.~~ Decided: token
   (D6). Gaps 1 and 2 stay in v1.
5. ~~Rerun the Gap 0 probe under the live-mode seal.~~ Done 2026-10-02 in the
   MUR concierge's seal: both are profile grants (table under Gap 0).
   Remaining: confirm the same grants are what the `e2e_browser_live` agent
   needs (`scripts/e2e/browser-live-f1.sh`). The recorder WARN is the
   expected F2 deny (see Gap 0). The single >600 s hang is unexplained; the
   test has per-step timeouts (`STEP_TIMEOUT`, 90 s) and, since 2026-10-02,
   a whole-run deadline (default 300 s, `MUR_BROWSER_E2E_DEADLINE_SECS`)
   that fails the test loudly instead of hanging; a forced 1 s deadline
   failed in 1.00 s. If the hang recurs, the panic now marks it.
