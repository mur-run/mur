# Anthropic OAuth Token Support for mur-core

## Background

Anthropic API supports two authentication methods:
1. **Regular API keys** (`sk-ant-api03-*`) — standard `x-api-key` header
2. **OAuth tokens** (`sk-ant-oat01-*`) — from Claude Code subscription, stored in macOS Keychain

OAuth tokens (from Claude Max/Pro subscription) require different handling than regular API keys.

## What OAuth Tokens Require

### 1. Auth Header Change

Regular API key:
```
x-api-key: sk-ant-api03-xxx
```

OAuth token:
```
Authorization: Bearer sk-ant-oat01-xxx
```

### 2. Beta Header

OAuth requests must include:
```
anthropic-beta: claude-code-20250219,oauth-2025-04-20
```

### 3. Billing Header in System Prompt

OAuth requests must prepend this to the system prompt:
```
x-anthropic-billing-header: cc_version=2.1.77; cc_entrypoint=sdk-cli;
```

### 4. Token Refresh from macOS Keychain

OAuth tokens expire daily. Claude Code auto-refreshes them and stores in macOS Keychain.
Read fresh token with:
```bash
security find-generic-password -s "Claude Code-credentials" -w
```

Returns JSON:
```json
{
  "claudeAiOauth": {
    "accessToken": "sk-ant-oat01-..."
  }
}
```

## Modified File

**`mur-core/src/llm.rs`** — the `anthropic_complete()` function

### Added Functions

- `is_anthropic_oauth_token(key: &str) -> bool` — detects `sk-ant-oat` prefix
- `read_oauth_from_keychain() -> Option<String>` — reads fresh token from macOS Keychain (cfg-gated for macOS only)

### Added Constants

- `BILLING_HEADER` — billing string prepended to system prompt
- `ANTHROPIC_OAUTH_BETAS` — beta features header value

### Logic in `anthropic_complete()`

```rust
if is_oauth {
    // 1. Try Keychain for fresh token, fallback to provided key
    effective_key = read_oauth_from_keychain().unwrap_or(api_key.to_string());
    // 2. Prepend billing header to system prompt
    system_final = format!("{}\n{}", BILLING_HEADER, system);
    // 3. Use Bearer auth + beta header
    req = req.header("Authorization", format!("Bearer {}", effective_key))
             .header("anthropic-beta", ANTHROPIC_OAUTH_BETAS);
} else {
    // Regular API key — use x-api-key as before
    req = req.header("x-api-key", api_key);
}
```

## Reference: mur-commander Implementation

The same pattern was implemented in `mur-commander` across multiple commits:

| Commit | Description |
|--------|-------------|
| `01bc8b3` | Added billing header to `call_llm_inner` and `call_llm_resolved` |
| `938a454` | Auto-refresh OAuth token from Keychain on 401 |
| `c293a5e` | Prefer Keychain OAuth token over stale `.env` token |

Key files in mur-commander:
- `crates/gateway/src/unified_handler/llm_service/provider.rs` — `BILLING_HEADER`, `is_anthropic_oauth_token()`, `read_oauth_from_keychain()`, `anthropic_auth_headers()`
- `crates/gateway/src/unified_handler/llm_service/mod.rs` — `call_llm_inner`, `call_llm_resolved`
- `crates/gateway/src/unified_handler/llm_service/agentic.rs` — `call_llm_agentic`, `call_llm_agentic_stream`
- `crates/gateway/src/unified_handler/llm_service/resolver.rs` — `resolve_api_key()` prefers Keychain for OAuth tokens

## `.env` Configuration

`~/.mur/.env`:
```
ANTHROPIC_API_KEY=sk-ant-oat01-xxx
```

If the key starts with `sk-ant-oat`, mur will automatically use OAuth flow. Regular `sk-ant-api03-` keys work unchanged.
