# Webhook receiver (Track C5)

The agent runtime can listen on an HTTP port for `POST /agents/<slug>/webhook` requests with HMAC-signed bodies. Use case: any external system that can `curl` — GitHub Actions, Zapier, n8n, iOS Shortcuts, a sibling mur-commander instance — can deliver text / URLs / files / images into the agent's inbox with the same B0 `<untrusted_share>` wrapping + Rule-4 cooldown the local Track C3 channels get.

## When to use it

- **CI build summaries**: a GitHub Action posts the failing test diff so the agent sees it on the next turn.
- **Cross-host A2A fallback**: when Noise XK isn't reachable (e.g. the peer's behind a strict firewall), HMAC-signed HTTP works.
- **Mobile capture**: an iOS Shortcut with the share-sheet output → cURL into the user's home Tailscale address.

## When NOT to use it

- **Public internet**: bind to localhost (default) or a VPN interface. Public-internet exposure means front-proxying the listener with a TLS terminator + WAF; we don't ship that. Route through Tailscale / WireGuard / SSH tunnel instead.
- **High-throughput streaming**: one POST = one ack. WebSocket / SSE join the v3 cross-host A2A track.
- **File uploads > 10 MiB**: hard 413 cap. Big files want multipart streaming, not base64-in-JSON; that's a v3 follow-up.

## Setup

```bash
# 1. Set the HMAC secret (hidden prompt; OS keychain storage)
mur agent webhook secret-set coach
# Enter HMAC secret for coach (input hidden): <type a strong key>
# Wrote mur-agent/coach/WEBHOOK_HMAC

# 2. Enable the listener (default 127.0.0.1:6789)
mur agent webhook enable coach
# Enabled webhook for coach: http://127.0.0.1:6789/agents/coach/webhook
# HMAC secret ref: mur-agent:coach/WEBHOOK_HMAC
# Run `mur agent webhook secret-set coach` … then restart the agent.

# 3. Restart the agent so the supervisor reads the new config
mur agent restart coach   # or kill + relaunch via your usual flow
```

Confirm the listener is up:

```bash
$ mur agent webhook show coach
enabled: true
bind: 127.0.0.1
port: 6789
hmac_secret_ref: mur-agent:coach/WEBHOOK_HMAC
```

`running.lock` will carry `transports.webhook = "http://127.0.0.1:6789"` so peers and the commander can discover the live URL without re-reading `profile.yaml`.

## Sending — curl

The HMAC is sha256 over the raw body. With `openssl`:

```bash
SECRET='your-strong-key'
BODY='{"kind":"text","value":"build #1234 failed: src/lib.rs:142"}'
SIG="sha256=$(printf '%s' "$BODY" | openssl dgst -sha256 -hmac "$SECRET" -binary | xxd -p -c 256)"

curl -sS -X POST http://127.0.0.1:6789/agents/coach/webhook \
    -H "X-Mur-Signature: $SIG" \
    -H 'X-Mur-Source: github-actions/myrepo' \
    -H 'Content-Type: application/json' \
    -d "$BODY"
# {"id":"4d8e…","queued_at":"2026-05-05T14:30:00.123Z"}
```

The agent's next turn sees the body wrapped in `<untrusted_share>`, with Rule-4 (after-untrusted-input) gating side-effect tools for one turn.

## Sending — GitHub Actions

```yaml
- name: Notify mur agent
  if: failure()
  env:
    MUR_HMAC: ${{ secrets.MUR_WEBHOOK_HMAC }}
  run: |
    BODY="{\"kind\":\"text\",\"value\":\"build ${{ github.run_id }} failed at ${{ github.sha }}\"}"
    SIG="sha256=$(printf '%s' "$BODY" | openssl dgst -sha256 -hmac "$MUR_HMAC" -binary | xxd -p -c 256)"
    curl -sS -X POST $MUR_WEBHOOK_URL \
        -H "X-Mur-Signature: $SIG" \
        -H 'X-Mur-Source: github-actions' \
        -H 'Content-Type: application/json' \
        -d "$BODY"
```

The `X-Mur-Source` header scopes the rate-limit bucket: 60 requests per minute per source by default. Many CI runs sharing one source key still share one bucket; for stricter isolation give each pipeline its own source string.

## Payload shape

```jsonc
{
  "kind": "text" | "url" | "image" | "file",
  "value": "<text/url, or base64-url-no-pad bytes for image/file>",
  "metadata": {
    "filename": "screenshot.png",   // optional; informs mime-sniff for image/file
    "source_hint": "ci-build-1234"  // optional; free-form, surfaces in telemetry
  }
}
```

## Error matrix

| Status | Meaning |
|---|---|
| 202 | Accepted; body sha256 in `id`, RFC3339 timestamp in `queued_at` |
| 401 | Missing or wrong `X-Mur-Signature` |
| 404 | Slug in path doesn't match this listener's agent |
| 413 | Body > 10 MiB |
| 422 | Body isn't valid JSON or `kind` isn't one of `text`/`url`/`image`/`file` |
| 429 | Rate limit exceeded (60/min per source by default) |
| 500 | Pipeline dispatch failed (disk error, mime decode failure) — logs carry the detail |

## Acceptance gate

```bash
bash scripts/e2e/c5-webhook.sh
```

Runs the harness suites:

- `mur-common` — `transport.webhook` round-trips through serde
- `mur-core` — CLI drift guards (default port + bind = localhost)
- `mur-agent-runtime` — 20 webhook tests covering the handler, HMAC verifier, pipeline dispatch, and token-bucket limiter

Bundle-level QA (signed bundle, real port bind, curl from outside) is a manual matrix — see "Manual matrix" below.

## Manual matrix

After landing all 6 milestones (M5.1 → M5.6) on main:

- [ ] `mur agent webhook secret-set` rejects an empty value (smoke test)
- [ ] `mur agent webhook enable` writes `profile.yaml` atomically (no half-written file when the rename fails)
- [ ] Restart agent, confirm `running.lock` has `transports.webhook` populated
- [ ] `curl` with valid signature → 202; body lands in `telemetry/inputs.jsonl` with `source: webhook`
- [ ] `curl` with wrong signature → 401
- [ ] `curl` from outside the bind interface (when bind=127.0.0.1) → connection refused
- [ ] Rapid-fire 100 `curl`s → ~60 succeed, rest 429
- [ ] `mur agent webhook disable` stops the listener on next agent restart
- [ ] After delete: `keychain` entry survives disable (re-enable doesn't need re-set)
