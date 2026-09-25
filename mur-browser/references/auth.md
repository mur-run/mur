# Browser auth — human-in-the-loop login, encrypted profile

Create or refresh a named browser profile by handing the login to a human
in a headed browser. MUR captures the resulting Playwright storageState,
encrypts it with age (mode 0600), and writes metadata only after the state
is saved. Other browser modes (`testing`, `automation`) must call this mode
first whenever they need an authenticated profile.

## Three rules that must never be broken

1. **The agent never enters credentials.** No typing usernames, passwords,
   OTPs, or answering security questions — not into the browser, not into
   a prompt, not via `{{secret:...}}` expansion. `mur browser auth` opens a
   headed browser and waits; the *person* signs in, then presses Enter in
   the terminal. If a task seems to need the agent to log in itself, stop
   and hand off to the human.
2. **Non-interactive contexts fail fast when the profile is missing.** If
   there is no human at the terminal (scheduled job, fleet run, piped stdin,
   any headless invocation) and `~/.mur/browser/profiles/<site>/state.json.age`
   does not exist, report the error and stop. Do **not** start
   `mur browser auth` on your own — the login handoff requires a person.
3. **Always pass `--browser <engine>`.** Without it, `select_browser`
   detects installed engines and, when more than one is present, prompts
   interactively on stderr/stdin. That prompt hangs or fails in an agent
   session. Pick an engine explicitly every time.

## Command table

Copied from `mur-core/src/cli/actions.rs` (`BrowserAction::Auth`) and
`mur-core/src/cmd/browser/mod.rs` (`BrowserEngine`). Do not invent flags.

| Command | Purpose |
|---|---|
| `mur browser auth <site> --url <URL> --browser <engine>` | First-time login for profile `<site>` |
| `mur browser auth <site> --url <URL> --browser <engine> --reauth` | Replace an existing profile's state after a fresh login |
| `mur browser status` | List profiles with their state and recorded runs |

`mur browser auth` parameters:

| Parameter | Kind | Meaning |
|---|---|---|
| `<site>` | positional, required | Profile name; used as a path component under `~/.mur/browser/profiles/`. Must satisfy `validate_name`: `[A-Za-z0-9._-]`, max 64 chars, must not start with `.` |
| `--url <URL>` | required | Login page to open in the headed session. Must be an absolute `http(s)` URL with a host |
| `--reauth` | flag | Replace an existing profile state. Without it, the command refuses when `state.json.age` already exists |
| `--browser <engine>` | optional value (**always pass it**, rule 3) | One of `chrome`, `chromium`, `firefox`, `msedge` |
| `--allow-domain <DOMAIN>` | optional, repeatable | Domain the profile may navigate to; subdomains included. Must be a bare host (no scheme, port, path, or wildcard). Defaults to the host of `--url` |

`--browser` value domain (`BrowserEngine`, clap `ValueEnum`):

| Value | Engine |
|---|---|
| `chrome` | Google Chrome |
| `chromium` | Chromium |
| `firefox` | Mozilla Firefox |
| `msedge` | Microsoft Edge |

## Procedure

1. Confirm a human is present and can see the terminal. If not → rule 2.
2. Run `mur browser status` and read the `profiles:` line for `<site>`.
3. Decide (see "When to reauth" below):
   - profile absent → first-time `mur browser auth <site> --url <URL> --browser <engine>`
   - profile present and healthy → nothing to do; hand the profile name to
     the calling mode
   - profile present but stale/broken → same command with `--reauth`
4. Tell the person what will happen: a browser window opens on `<URL>`,
   they sign in themselves, then press Enter in the terminal. The CLI
   prints: `Complete sign-in in the opened browser, then press Enter here to save the profile.`
5. Wait for the CLI to print `saved encrypted browser profile "<site>"`.
   Anything else is a failure; report the error text verbatim and do not
   retry automatically.
6. Re-run `mur browser status` and confirm `<site>` now shows
   `(cookie expires …)` or `(authenticated …)`.

## When to reauth

Run with `--reauth` when `mur browser status` shows any of:

- `<site> (incomplete)` — the profile directory exists but
  `state.json.age` is missing (a previous login was interrupted before
  encryption finished).
- `<site> (metadata missing)` — state exists but `meta.yaml` is missing;
  the profile cannot be trusted.
- `<site> (cookie expires <timestamp>)` where the timestamp is already in
  the past — `earliest_cookie_expires` has elapsed, so the session is
  likely dead.

Also reauth when a downstream mode reports that a run landed on a login
page while using this profile — but only with a human present (rule 2).

A profile showing `(authenticated <timestamp>)` has no cookie expiry
recorded; treat it as valid until a downstream mode observes a logout.

## Reading `mur browser status`

```
profiles: pchome (cookie expires 2026-10-01 00:00:00 UTC), shopee (incomplete)
runs: (none)
```

Each profile label is one of: `(incomplete)`, `(metadata missing)`,
`(cookie expires <ts>)`, `(authenticated <ts>)`. `(none)` means no
profiles at all.

## Failure modes you may see

| Message | Cause | Action |
|---|---|---|
| `invalid name "...": use [A-Za-z0-9._-], max 64 chars, not starting with '.'` | `<site>` failed `validate_name` | Choose a compliant name |
| `--url must be an absolute http(s) URL` | bad `--url` | Fix the URL |
| `browser profile "<site>" already has encrypted state; use --reauth to replace it` | state exists, no `--reauth` | Confirm with the human, then add `--reauth` |
| `no supported browser found. Install Chrome, Chromium, Firefox, or Microsoft Edge, then retry with --browser <engine>` | no engine installed | Ask the human to install one |
| `login confirmation cancelled: stdin closed` | no interactive stdin | You violated rule 2 — stop |
| `browser selection cancelled: stdin closed` | `--browser` omitted in a non-interactive session | You violated rule 3 — add `--browser` |

## What this mode produces

- `~/.mur/browser/profiles/<site>/state.json.age` — age-encrypted
  Playwright storageState, mode 0600.
- `~/.mur/browser/profiles/<site>/meta.yaml` — `last_auth`, login URL,
  `earliest_cookie_expires`; written only after the state file is saved.

Downstream modes reference the profile by `<site>` name only. Never read,
decrypt, print, or copy `state.json.age`; the raw storageState must not
enter agent context.

## Handing off to testing / automation

Before either mode uses `--profile <site>`, run this mode's procedure and
confirm `mur browser status` shows `<site>` as healthy. Pass forward only:
the profile name, the engine you used, and the status line. Nothing else.
