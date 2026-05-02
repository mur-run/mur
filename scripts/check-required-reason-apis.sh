#!/usr/bin/env bash
# scripts/check-required-reason-apis.sh
# Fails when source code uses an Apple Required Reason API category
# that is NOT declared in mur-agent-gui/src-tauri/Resources/PrivacyInfo.xcprivacy.
#
# We grep for category-implicating keywords in our Rust + TS sources,
# then assert each match's category is present in the manifest.
#
# Categories tracked here are those the Required Reason API list
# specifically calls out:
#   UserDefaults     — std::collections::UserDefaults-equivalent
#   FileTimestamp    — fs::Metadata::modified / created
#   SystemBootTime   — Instant::now / mach_absolute_time / sysctl kern.boottime
#   DiskSpace        — statvfs / NSFileManager attributesOfFileSystem
#   ActiveKeyboard   — UIKit text-input prefs  (NOT used in mur — flagged for safety)
#   PeripheralAccess — IOKit  (NOT used in mur — flagged for safety)
#
# The gate prints which file mentions which category, then bails if
# the manifest is missing the corresponding entry.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MANIFEST="$REPO_ROOT/mur-agent-gui/src-tauri/Resources/PrivacyInfo.xcprivacy"

if [ ! -f "$MANIFEST" ]; then
    echo "ERROR: manifest missing at $MANIFEST" >&2
    exit 1
fi

# (regex, category) pairs — extend when we adopt a new RR API surface.
declare -a CHECKS=(
    'fs::metadata|\.modified\(\)|\.created\(\)|MetadataExt::mtime|file_modification_date'
    'NSPrivacyAccessedAPICategoryFileTimestamp'

    'Instant::now|mach_absolute_time|kern\.boottime|systemUptime|ProcessInfo\.processInfo\.systemUptime'
    'NSPrivacyAccessedAPICategorySystemBootTime'

    'statvfs|attributesOfFileSystem|disk_space|fileSystemFreeSize|free_disk_space'
    'NSPrivacyAccessedAPICategoryDiskSpace'

    'UserDefaults|tauri_plugin_store|store\.set|store\.get'
    'NSPrivacyAccessedAPICategoryUserDefaults'

    'UITextInputMode\.activeInputModes|preferredLanguages'
    'NSPrivacyAccessedAPICategoryActiveKeyboards'

    'IOServiceMatching|IOPSCopyPowerSourcesInfo|IORegistryEntry'
    'NSPrivacyAccessedAPICategoryPeripheralAccess'
)

# Crates we ship in the desktop app. The runtime (mur-agent-runtime)
# is a sidecar bundled with the GUI, so its source counts.
SCAN_DIRS=(
    "mur-agent-gui/src-tauri/src"
    "mur-agent-gui/ui/src"
    "mur-core/src"
    "mur-common/src"
    "mur-agent-runtime/src"
)

violations=0

for ((i=0; i<${#CHECKS[@]}; i+=2)); do
    pattern="${CHECKS[$i]}"
    category="${CHECKS[$i+1]}"

    matches=$(grep -REn "($pattern)" "${SCAN_DIRS[@]}" \
        --include="*.rs" --include="*.ts" --include="*.tsx" \
        2>/dev/null || true)

    if [ -z "$matches" ]; then
        continue
    fi

    if grep -q "$category" "$MANIFEST"; then
        # Category is declared — usage is fine. Print for transparency.
        echo "OK: $category — declared, used in:"
        echo "$matches" | sed 's/^/    /'
        echo
    else
        echo "VIOLATION: $category — used but NOT declared in PrivacyInfo.xcprivacy:" >&2
        echo "$matches" | sed 's/^/    /' >&2
        echo >&2
        violations=$((violations+1))
    fi
done

if [ "$violations" -gt 0 ]; then
    echo "$violations Required Reason API category violation(s)." >&2
    echo "Fix by either:" >&2
    echo "  1. Adding the missing NSPrivacyAccessedAPI<Category> entry to the manifest" >&2
    echo "     with the appropriate reason code, OR" >&2
    echo "  2. Removing the API usage." >&2
    exit 1
fi

echo "Required Reason API gate: clean."
