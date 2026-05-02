# Companion → GUI Bridge (D5)

Every proactive companion message the runtime writes to
`~/.mur/agents/<name>/companion/inbox/<id>.md` is delivered to the
desktop app within ≤ 1 s — even when the main window is hidden — and
shows up as a desktop notification, a dock badge, and a sidebar row
with inline 👍 / 👎 / 🚫 buttons.

## Pipeline

1. **Source of truth** — the runtime's outbox tick writes a markdown
   file via `StdoutNotifier::send` (`O_CREAT | O_EXCL`, atomic). The
   file's front-matter carries `id`, `situation`, `template_id`,
   `locale`, `generated_at`. The trailing line is
   `>>> response: <unset>` until the user acks.
2. **Watcher** — on launch, the GUI scans the inbox dir
   (`companion_bridge::scanner::scan_pending`) so a restart never
   loses pending messages. It then attaches a `notify` watcher
   (`companion_bridge::watcher::InboxWatcher`) on `Create` events.
3. **Tauri 2 Channel** — every event flows through a typed
   `Channel<BridgeEvent>` returned by `companion_bridge_subscribe`.
   We deliberately use channels instead of `emit_to`: channels
   deliver reliably even when the webview is minimized
   (Tauri #11811).
4. **React** — `useCompanionBridge(agent)` calls
   `tauri-plugin-notification::sendNotification` and
   `Window::set_badge_count` for every event. The sidebar shows the
   running unread count + the last N messages.
5. **Ack** — pressing 👍/👎/🚫 invokes `companion_ack`, which atomically
   rewrites the `>>> response:` line. The runtime's outbox loop picks
   the new value up on its next scan and feeds it back through the
   bandit picker.
6. **Why** — the `Why did you message?` accordion calls
   `companion_why` to load every ledger entry whose `id` matches
   the message — typically `MessageScheduled → MessageGenerated → MessageSent`.
   The ledger lives at `companion/outbox-ledger/<YYYY-MM-DD>.jsonl`
   (per-day JSONL files, scanned via the runtime's `Ledger::scan_days`).
7. **Quiet / proactive** — header toggles call the existing
   `companion proactive` and `companion quiet` CLI verbs via thin
   Tauri command wrappers.

## Acceptance gates

- `mur-agent-gui/src-tauri/tests/bridge_acceptance.rs` —
  write-to-event latency < 1 s.
- `mur-agent-gui/src-tauri/tests/bridge_*` —
  parse, scan, watch, state, notify, ack, why.
- `scripts/e2e/v1-d5-gui-bridge.sh` runs all of the above in
  release mode.

## Privacy

The bridge is local-only. The watcher reads `companion/inbox/*.md`
which is owned by the user. No network traffic. No telemetry beyond
the existing companion ledger.
