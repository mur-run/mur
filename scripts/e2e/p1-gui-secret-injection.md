# Manual acceptance — GUI secret injection

This step proves Task 4.1: when `Kelp.app` is launched from Finder, the
Tauri main process resolves the active model's secret and injects it via
`Command::env()` into the sidecar — bypassing the "GUI launch falls back to
echo because the shell env wasn't inherited" failure mode.

Cannot be automated end-to-end because it requires:
- A real Anthropic API key (so the sidecar can actually call the API).
- A real `.app` (so we can launch it via Finder, which strips the user's
  shell env).

## Steps

1. Pick an agent name and provider. Throughout the steps below, replace
   `kelp` with whatever you choose.

   ```bash
   mur agent create kelp --no-interactive --provider anthropic --model claude-opus-4-7
   ```

2. Register the model in the shared registry, pointing the secret at the
   keychain entry the next step writes:

   ```bash
   mur model add anthropic_test \
     --provider anthropic \
     --model claude-opus-4-7 \
     --secret keychain:mur-agent/kelp/ANTHROPIC_API_KEY
   ```

3. Wire the agent's profile to the registry entry:

   ```bash
   echo 'model_ref: anthropic_test' >> ~/.mur/agents/kelp/profile.yaml
   ```

4. Save your real API key into the OS keychain:

   ```bash
   mur agent secret kelp set ANTHROPIC_API_KEY
   # paste the real sk-... value at the prompt
   ```

5. Export the agent as a click-to-launch app:

   ```bash
   mur agent export kelp --format gui --out ~/Desktop/Kelp.app --skip-notarize
   ```

6. Strip the macOS quarantine bit (because we used --skip-notarize) and
   open from Finder — **not** from a terminal that already has
   `ANTHROPIC_API_KEY` exported, because that would mask the bug we're
   testing for.

   ```bash
   xattr -dr com.apple.quarantine ~/Desktop/Kelp.app
   open ~/Desktop/Kelp.app
   ```

7. **Check the Model tab.** Navigate to the Model tab — confirm:
   - The list shows `anthropic_test` (and any other registry entries).
   - The active radio button is on `anthropic_test`.
   - The status pill reads "✓ ready" (i.e. `secret.check()` resolved
     the keychain item written in step 4).

   If the secret pill reads "✗ secret not set" instead, click the
   "Update secret" button and paste the value into the modal. This is
   the in-app shortcut for step 4 — useful when the user installed the
   .app on a different machine.

8. **Send a real chat.** From the Status tab, send a message — or, in a
   *clean* shell with **no** `ANTHROPIC_API_KEY` set, run
   `mur agent send kelp 'hello'`.

## Expected

A real Claude reply appears — no `echo:` prefix, no "anthropic client
unavailable" warning in the log pane.

## What this proves

- The Tauri main resolved `profile.model_ref` → `~/.mur/models.yaml` →
  `entry.secret` → the keychain item.
- The resolved value flowed into the sidecar via `Command::env()`, so the
  sidecar's existing `AnthropicClient::from_env()` path picked it up
  unchanged.
- No env-var leak into the parent shell or the Finder launch context.
