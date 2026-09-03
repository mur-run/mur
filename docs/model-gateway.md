# Model gateway

`mur-model-gateway` is a small local proxy (`127.0.0.1:8088`) that attaches
the credentials of subscriptions you already pay for to requests that arrive
without one. Its own README covers installation, routing, and compression:
<https://github.com/mur-run/mur-model-gateway>. This page covers how MUR uses
it for a **ChatGPT Subscription** and what that means for billing.

## ChatGPT Subscription

Use a ChatGPT Plus/Pro plan for MUR agents through Codex, with no API key and
no usage billing.

Who owns what:

| Concern | Owner |
|---|---|
| Sign-in and the credential on disk | Codex CLI (`codex login` / `codex logout`) |
| Account and model status | MUR Hub, via a short-lived `codex app-server` session |
| Attaching the token, refreshing it, translating to the Responses API | `mur-model-gateway` |
| Registry entries and agent assignment | MUR |

MUR never reads, parses, logs, or copies the Codex credential file. The Hub
only ever sees "signed in as *email* on plan *X*" and the list of model ids.

### Setup

1. Install Codex CLI (`npm install -g @openai/codex`) and the gateway
   (`brew install mur-run/tap/mur-model-gateway`).
2. In MUR Hub, open **Models → Model Library → Add Provider → ChatGPT
   Subscription**. This is a separate entry from **OpenAI**, which is the
   usage-billed API.
3. **Sign in with ChatGPT.** Codex opens the browser flow; MUR waits for it
   and then asks Codex which account is signed in. Exit code alone is not
   trusted.
4. **Install gateway.** The Hub asks for consent first, then runs
   `mur-model-gateway install --token-source-codex codex`, loads the service,
   and polls the gateway's health endpoint. Existing compression settings are
   preserved.
5. Pick the models to add. Your plan's default model is pre-selected. Each
   becomes a registry entry with `provider: codex`, `billing: subscription`,
   and no `secret`.

From the CLI the same entry looks like this in `~/.mur/models.yaml`:

```yaml
models:
  chatgpt_gpt_5_6_sol:
    provider: codex
    model: gpt-5.6-sol
    base_url: http://127.0.0.1:8088/codex/v1
    tier: frontier
    billing: subscription
    catalog_verified: true
```

### The fixed route and no-key behaviour

`provider: codex` sends OpenAI Chat Completions requests **without any
credential** to `http://127.0.0.1:8088/codex/v1`. The runtime accepts only
that shape: `http`, `localhost` or a loopback IP, an explicit port, the path
`/codex/v1`, no user info, query, or fragment. A remote host, an `https`
URL, or a `secret` on the entry is refused at startup rather than sent —
an authless request to a stranger, or a route that quietly lands on OpenAI
Platform billing, is not something a typo should be able to produce.

The gateway attaches the ChatGPT OAuth token itself and translates the call to
the Responses API upstream. `GET http://127.0.0.1:8088/__mur/health` reports
`codexHook`, `codexCredential` (`chatgpt`, `apikey`, or `missing` — a kind,
never a token), and `compression`; the Hub treats only `chatgpt` as ready.

### Subscription vs usage billing

- **ChatGPT Subscription** (`provider: codex`) — covered by your plan. Rate
  limits are the plan's limits.
- **OpenAI** (`provider: openai`) — your API key, billed per token. Unchanged
  by this feature and still requires a key.

Model pickers and fallback-chain rows show a billing label. Entries written
before billing metadata existed show **Billing unknown**; unknown is never
rendered as free.

### Rate limits (429) and fallbacks

A 429 from the subscription does **not** grant MUR permission to spend
money. MUR never inserts a usage-billed OpenAI model into a ChatGPT model's
fallback chain on its own. If you add one yourself, the chain editor warns
that the fallback is usage-billed (or that its billing is unknown) and keeps
the row editable; saving it is your explicit choice.

### Model discovery failed?

If `model/list` cannot be read, the panel offers **Advanced: add an
unverified model ID**. The entry is written with `catalog_verified: false`
and shows an **Unverified** badge. No static model list is substituted.

### Disconnect vs sign out

- **Disconnect MUR** removes the subscription entries (`provider: codex` with
  `billing: subscription`) from MUR's registry. It does not touch the Codex
  login, the gateway, or hand-written codex entries. `codex login status`
  still reports you signed in.
- **Sign out of ChatGPT** runs `codex logout` after a confirmation, because it
  signs out **every** Codex client on the machine — Codex CLI and IDE
  extensions included. Registry entries that remain become unhealthy until
  you sign in again.

## Claude Subscription

Use a Claude Pro/Max plan for MUR agents through Claude Code, with no API
key and no usage billing. Same shape as ChatGPT Subscription above; the CLI
is Claude Code and the route is `/v1`.

Who owns what:

| Concern | Owner |
|---|---|
| Sign-in and the credential on disk | Claude Code (`claude auth login` / `logout`) |
| Account status | MUR Hub, via `claude auth status --json` |
| Attaching and refreshing the token | `mur-model-gateway` |
| Registry entries and agent assignment | MUR |

MUR never reads or copies the Claude Code credential (keychain blob or
`~/.claude/.credentials.json`). The Hub keeps three fields from
`claude auth status` — whether you are signed in, which login kind, and the
email — and drops everything else.

### Setup

1. Install Claude Code (`npm install -g @anthropic-ai/claude-code`) and the
   gateway (`brew install mur-run/tap/mur-model-gateway`).
2. In MUR Hub, open **Models → Model Library → Add Provider → Claude
   Subscription**. This is a separate entry from **Anthropic**, which is the
   usage-billed API.
3. **Sign in with Claude.** The Hub runs `claude auth login --claudeai` and
   then asks `claude auth status` who is signed in; exit code alone is not
   trusted. A **Console** login (`authMethod: console`) is the Anthropic API
   and reads as "signed in, but not this provider".
4. **Install gateway** if it is not running, behind the same consent card.
5. Pick models. They come from the models.dev catalog, not from a live
   endpoint probe.

The registry entry:

```yaml
models:
  claude_opus_5:
    provider: claude
    model: claude-opus-5
    base_url: http://127.0.0.1:8088/v1
    tier: frontier
    billing: subscription
    catalog_verified: true
```

### The fixed route and no-key behaviour

`provider: claude` sends Anthropic Messages requests **with no `x-api-key`
header at all** to `http://127.0.0.1:8088/v1`. Absent, not empty: the
gateway decides what to do by whether an auth header is present. No header
means "attach the Claude Code token"; an empty one would mean "pass this
through untouched" and earn a 401.

The runtime accepts only that shape — `http`, a loopback host, an explicit
port, the path `/v1`, no user info, query, or fragment. A remote host, an
`https` URL, or a `secret` on the entry is refused at startup rather than
sent. `GET http://127.0.0.1:8088/__mur/health` reports `claudeCredential`
(`oauth` or `missing` — a kind, never a token); the Hub treats only `oauth`
as ready.

### Already using `provider: anthropic` with the gateway?

It keeps working, unchanged. The difference is what the entry guarantees:
`provider: anthropic` is the same route with no protection, so one
`base_url` edit to `api.anthropic.com` turns it into a metered API entry
and nothing objects. Switching is two lines — `provider: claude` and
`base_url` with `/v1` appended.

`mur model doctor` points at the entries that could carry the label. It
stays quiet about an `anthropic` gateway entry that has a `secret`, because
what that entry does depends on the token in the secret (an `sk-ant-oat`
subscription token rides the gateway; a normal API key is passed through and
billed), and doctor does not read secrets to find out.

### Disconnect vs sign out

- **Disconnect MUR** removes the `provider: claude` subscription entries from
  MUR's registry. `claude auth status` still reports you signed in, and every
  other Claude Code session keeps working.
- **Sign out of Claude** runs `claude auth logout` after a confirmation,
  because it signs Claude Code out everywhere on this machine — terminal
  sessions and IDE extensions included.

### References

- OpenAI, *Codex authentication*: <https://developers.openai.com/codex/auth>
- Anthropic, *Claude Code*: <https://docs.claude.com/en/docs/claude-code/overview>
- OpenAI, *Codex app-server*: <https://developers.openai.com/codex/app-server>
- Designs: `docs/superpowers/specs/2026-09-02-mur-hub-chatgpt-subscription-design.md`,
  `docs/superpowers/specs/2026-09-03-mur-hub-claude-subscription-design.md`
