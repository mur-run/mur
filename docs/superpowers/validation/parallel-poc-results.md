# Gate 0 PoC Results

**Date:** 2026-06-29  
**Script:** `scripts/parallel_poc.py`  
**Status:** BLOCKED — cc-proxy authentication failure

---

## Blocker: API Auth Unavailable in Python Subprocesses

The `scripts/parallel_poc.py` script could not be run. All Anthropic API calls (both
direct to `api.anthropic.com` and via the local cc-proxy at `http://127.0.0.1:8088`)
return HTTP 401:

| Attempted route | Result |
|---|---|
| `api.anthropic.com` with `~/.mur/secrets/anthropic.key` as `x-api-key` | 401 invalid x-api-key |
| `http://127.0.0.1:8088` with `~/.mur/secrets/anthropic.key` as `x-api-key` | 401 Invalid authentication credentials |
| `http://127.0.0.1:8088` with key as Bearer token | 401 Invalid authentication credentials |

Per the project memory (`env_cc_proxy_base_url.md`), raw Anthropic API keys do not
work directly in this environment — all traffic must route through the cc-proxy at
port 8088, which uses the Claude Code OAuth session for outbound auth. The cc-proxy
is running but its upstream session token is invalid (likely rotated by a recent
`/login` — see gotcha `gotcha_login_rotates_ccproxy_token_agents_401.md`).

Claude Code itself (the process running this session) is able to make API calls,
but the auth token is not exposed to Python subprocesses.

---

## What Was Completed

- [x] `pip install anthropic tree-sitter tree-sitter-rust` — all deps installed successfully
- [x] `scripts/parallel_poc.py` written verbatim from the task brief spec
- [ ] Script could not be executed — API auth blocker
- [ ] Results not collected
- [ ] Gate 0 PASS/FAIL cannot be determined

---

## Gate 0 Result

**GATE 0: BLOCKED (cannot determine PASS/FAIL)**

No results to report. The script logic is correct and matches the spec — the only
blocker is environment auth.

---

## Unblocking Options

**Option 1 (fastest):** Re-authenticate cc-proxy by running `claude` /login in a
terminal, which refreshes the cc-proxy upstream session. Then re-run:
```bash
ANTHROPIC_API_KEY=$(cat /Users/david/.mur/secrets/anthropic.key) \
  python3 scripts/parallel_poc.py
```

**Option 2:** Export a real Anthropic API key to the environment before running:
```bash
export ANTHROPIC_API_KEY=sk-ant-...
python3 scripts/parallel_poc.py
```

**Option 3:** Add `ANTHROPIC_API_KEY` to `~/.claude/settings.json` env block so it's
inherited by all subprocess tools in Claude Code sessions.

---

## Notes

The tree-sitter setup (`Language(tree_sitter_rust.language()); Parser(lang)`) was
verified syntactically against the installed `tree-sitter==0.24.x` / `tree-sitter-rust==0.24.2`
API, which uses the new `Language(binding)` constructor (not the legacy two-argument form).
This should work once auth is resolved.
