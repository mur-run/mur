# Spike: obscura as the research-gateway render tier (replace Lightpanda)

> **This is a time-boxed evaluation spike, not an implementation plan.** Its
> only deliverable is a **Go / No-Go decision** with evidence. No production
> code lands from the spike itself; if Go, a separate implementation plan
> follows.

**Question:** Can [obscura](https://github.com/h4ckf0r0day/obscura) replace
the `agent-browser` + Lightpanda render tier (tier 2/3) in
`mur-research-gateway`, giving us a JS-render path that actually works under
the kernel sandbox **and** routes its egress through our loopback proxy?

**Time-box:** 1 day. Kill on the first hard No.

---

## Why

The current render tier (`fetch(url, render=true)`) drives `agent-browser`
as a subprocess with two engines:

- **tier 2 — Lightpanda:** fast, keyless, but known flaky / half-dead
  (`gotcha_agent_browser_lightpanda_engine_dead`): needs a separately-installed
  binary, errors on stealth args, and its engine has been unreliable.
- **tier 3 — Chrome:** heavy, hardest to fit inside the kernel sandbox.

Both engines **open their own sockets the egress proxy cannot see**, so SSRF /
deny is enforced only by an *advisory* pre-spawn screen (`browser.rs` §5), with
a DNS-rebinding window left open (Phase 3 TODO).

obscura is a candidate that could fix both problems at once:

| obscura property | Why it matters here |
|---|---|
| `obscura fetch <url> --dump text\|markdown` → stdout | Near-drop-in for the `run_agent_browser` subprocess contract (URL in, rendered text out) |
| Explicit `--proxy http://…` / `socks5://…` flag | *If* it accepts proxy auth, the render tier could route through our loopback egress proxy — closing the advisory-only gap Lightpanda/Chrome can't |
| Self-contained: embeds V8, **no external Chrome / Node** | Fits the dependency-light gateway + sandbox better than the Chrome tier |
| Prebuilt binaries (macOS Intel/ARM, Linux x86/ARM, Windows), Apache-2.0 | Installs like Lightpanda (`~/.mur/aura/…`); license clean |
| 18.6k★, CDP-compatible | Mature enough to depend on (though still 0.x) |

**Non-goals (explicit):** this spike is about **rendering URLs we were
legitimately given** (`fetch`), NOT about search. Search's first-class path is
Brave/DDG via the tier-1 HTTP path (PR #674) — obscura does **not** touch it.
This is also NOT about evading any search engine's bot detection.

## What would change (the replacement point)

`mur-research-gateway/src/browser.rs`:
- `fetch_rendered(url, cfg, want_chrome, deny, timeout) -> Result<String, FetchError>`
  — screens the URL, builds argv, runs the engine subprocess under `timeout`,
  returns rendered text.
- `build_fetch_argv(url, cfg, want_chrome)` — engine selection + argv.
- `run_agent_browser(bin, argv, timeout)` — bounded subprocess exec.

The obscura variant is the same shape: `obscura fetch <screened_url> --dump
markdown [--proxy <loopback>]`, bounded by `timeout`, stdout captured. So *if*
the spike is Go, the integration is small and localized to `browser.rs` +
`config.rs` (new engine path + binary location). **Do not write that
integration during the spike** — only prove the questions below.

---

## Spike questions (ordered by decisiveness — kill on first hard No)

### Q1 — DECISIVE: does obscura's V8 render run under our kernel sandbox?

This is the wall Lightpanda/Chrome hit (the G2 saga). V8 needs JIT-writable /
then-executable memory (W^X); macOS SBPL and Linux Landlock may deny it.

**Method:**
- Install the obscura prebuilt binary. Confirm `obscura fetch https://example.com
  --dump text` works **unsandboxed** first (baseline).
- Then run the **same command inside the gateway's actual sandbox profile**
  (`mur-agent-runtime` SBPL on macOS / Landlock on Linux — the profile a real
  research worker's gateway child runs under). Reuse the exact profile the
  supervisor seals, not a hand-rolled one.
- Also try `obscura fetch … --v8-flags="--no-opt"` / `--jitless` if obscura or
  V8 exposes it — a jitless/interpreter-only mode may render (slower) where JIT
  is denied.

**Kills the spike if:** V8 cannot execute under the sandbox in any mode (no
JIT, no jitless fallback). Then no embedded-V8 engine helps, and the render
tier stays a known gap — STOP, document, done.

**Advances if:** it renders under the sandbox (JIT or jitless). Record which
mode + the latency cost of jitless if that's what's required.

### Q2 — governance: does `--proxy` accept our loopback proxy WITH auth?

Our egress proxy is a loopback HTTP proxy requiring `Proxy-Authorization:
Basic <token>` (per-server token). Lightpanda/Chrome can't use it → advisory
screen only. If obscura's `--proxy` honors auth, the render tier becomes
proxy-governed.

**Method:**
- Stand up the gateway's egress proxy (`sandbox/egress_proxy.rs`) with a known
  token. Run `obscura fetch <url> --proxy http://<token>:@127.0.0.1:<PORT>`
  (userinfo-in-URL is the usual way CLIs pass proxy Basic auth) and confirm the
  request appears in the proxy's audit with a valid `Proxy-Authorization`.
- If userinfo-in-URL isn't honored, check for an obscura proxy-auth flag/env.

**Outcome (not a killer):**
- **Yes** → record it; the implementation plan can route the render tier
  through the proxy = airtight egress for tier 2/3, a strict upgrade.
- **No** → fall back to the *same advisory pre-spawn screen* the current tier
  uses (no worse than today). Still viable, just not a governance win.

### Q3 — quality & perf vs Lightpanda, under concurrency

**Method:** on ~10 representative research targets (JS-heavy docs, a GitHub
README, a couple of vendor pages), compare `obscura fetch --dump markdown`
output against the current Lightpanda tier for: extraction quality (is the
main content there?), latency, and memory — run 4 concurrent (our worker
fan-out) and watch for degradation / crashes.

**Outcome (not a killer):** record a quality/perf verdict. obscura's
`--dump markdown` may also beat our naive `html_to_text` tag-strip.

### Q4 — footprint: install, binary size, CI, cross-platform

**Method:** confirm prebuilt binaries exist for every platform the gateway
ships on; measure binary size; confirm no new build-time dep leaks into the
dependency-light gateway crate (we shell out to a binary, we don't link it).
Decide the install location (mirror `~/.mur/aura/lightpanda` →
`~/.mur/aura/obscura`?) and how it's fetched.

**Outcome (not a killer):** a footprint note + install-story sketch.

---

## Go / No-Go

**Go** (proceed to an implementation plan) requires ALL of:
1. **Q1 = Yes** — renders under the real sandbox profile (JIT or jitless).
2. **Q3** — extraction quality ≥ Lightpanda and no crash/leak under 4× concurrency.
3. **Q4** — prebuilt binary for every shipped platform; footprint acceptable.

**Q2 is a bonus, not a gate:** proxy-auth working turns it into a governance
upgrade; not working means we keep today's advisory screen (no regression).

**No-Go** on any of: Q1 hard No (V8 can't run sandboxed), Q3 worse extraction
or instability under concurrency, or Q4 missing platform binaries. Document the
finding (especially a Q1 No — it's the definitive answer on embedded-V8 engines
under our sandbox) and stop.

## Deliverable

A short findings note appended to this file (or a sibling
`…-spike-obscura-render-tier-findings.md`) with the Q1–Q4 evidence and the
Go/No-Go call. If Go, a separate implementation plan under
`docs/superpowers/plans/` wires obscura into `browser.rs` as the tier-2 engine.
