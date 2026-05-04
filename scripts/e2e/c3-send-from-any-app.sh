#!/usr/bin/env bash
# scripts/e2e/c3-send-from-any-app.sh — Track C3 send-from-any-app
# acceptance.
#
# This PR series ships the harness for all four channels (URL scheme,
# global hotkey, macOS Services menu, drag-to-dock) plus the React
# composer integration. Production wiring (lib.rs::setup hooks for
# tauri-plugin-deep-link / global-shortcut, NSApplication.servicesProvider,
# RunEvent::Opened, App.tsx mount of startShareListener) lands in a
# follow-up. So this script gates the harness — every channel's
# parser / decoder / classifier and the SharePayload → SendIngestor
# seam — rather than building a real Tauri app and driving the
# native side. The plan's `--self-test=<mode>` binary modes ship
# alongside production wiring.
#
# Acceptance gates (roadmap §5.5):
#  1. SharePayload + ShareKind round-trip (M-c3.0.1)
#  2. SendIngestor routes Text/Url through process_share_text and
#     Image/File through process_artifact (M-c3.0.2)
#  3. B0SafetyHook wraps `--- share` sidecar bodies as
#     `<untrusted_share>` (M-c3.0.3)
#  4. Channel A — muragent-<slug>://share?... parser (M-c3.1)
#  5. Channel B — Cmd+Shift+M+<X> hotkey + clipboard synth (M-c3.2)
#  6. Channel C — NSPasteboard decoder + NSServices Info.plist rewrite
#     (M-c3.3, macOS-only)
#  7. Channel D — RunEvent::Opened classify_path + multi-URL ingest
#     (M-c3.4, macOS-only)
#  8. TS share handler + ShareBadge composer integration (M-c3.5)

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

UNAME="$(uname)"

echo "==> 1/8 mur-core unit + integration tests (M-c3.0 SendIngestor + B0 wrapping)"
cargo test -p mur-agent-runtime --quiet send -- --skip companion --test-threads=1

echo "==> 2/8 mur-core URL scheme injection (M-c3.1.4)"
cargo test -p mur-core --test agent_export_gui_url_scheme --quiet

if [[ "$UNAME" == "Darwin" ]]; then
    echo "==> 3/8 mur-core NSServices Info.plist injection (M-c3.3.3, macOS)"
    cargo test -p mur-core --test agent_export_gui_nsservices --quiet
else
    echo "==> 3/8 SKIP NSServices (macOS-only)"
fi

# `mur-agent-gui` is a workspace-EXCLUDED standalone crate; cd into
# it so cargo doesn't walk past the worktree's Cargo.toml.
cd "$REPO_ROOT/mur-agent-gui/src-tauri"

echo "==> 4/8 GUI URL scheme parser + MockApp (M-c3.1)"
cargo test --test send_url_scheme --quiet

echo "==> 5/8 GUI hotkey + clipboard synth + MockApp (M-c3.2)"
cargo test --test send_hotkey --quiet

if [[ "$UNAME" == "Darwin" ]]; then
    echo "==> 6/8 GUI NSPasteboard decoder + MockApp (M-c3.3, macOS)"
    cargo test --test send_services --quiet

    echo "==> 7/8 GUI dock classify_path + simulate_opened (M-c3.4, macOS)"
    cargo test --test send_dock --quiet
else
    echo "==> 6/8 SKIP services (macOS-only)"
    echo "==> 7/8 SKIP dock (macOS-only, also skipped on cross-platform)"
fi

cd "$REPO_ROOT/mur-agent-gui/ui"

echo "==> 8/8 TS share handler + ShareBadge composer integration (M-c3.5)"
if [[ ! -d node_modules ]]; then
    echo "    npm ci (first run)"
    npm ci --silent
fi
npm test --silent -- --run

cd "$REPO_ROOT"
echo
echo "OK — Track C3 acceptance gates passed (harness-level)."
echo "Production wiring (lib.rs::setup hooks + App.tsx mount + --self-test"
echo "binary modes) lands in a follow-up alongside the manual native-channel"
echo "QA matrix described in docs/cookbook/c3-send-from-any-app.md."
