#!/bin/bash
# Behavioral tests for scripts/codesign.sh — the one shared signing step used by
# build.sh (and, by hand, after a plain `cargo build`).
#
# Tested only through the public seam: run the script on a binary, then read
# the signature back with `codesign -d`. No keychain identity needed — the
# default (MUR_CODESIGN_IDENTITY unset) is ad-hoc, which is what CI has.
# macOS only; exits 0 with a notice elsewhere.
set -euo pipefail

if [ "$(uname)" != "Darwin" ]; then
  echo "test-codesign: not macOS, skipping"
  exit 0
fi

REPO="$(cd "$(dirname "$0")/.." && pwd)"
SCRIPT="$REPO/scripts/codesign.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

FAILS=0
fail() { echo "FAIL: $*"; FAILS=$((FAILS + 1)); }
pass() { echo "ok:   $*"; }

identifier_of() {
  codesign -d --verbose=2 "$1" 2>&1 | sed -n 's/^Identifier=//p'
}

# --- test: signed binary gets a stable identifier named after the file -------
# cargo's linker signature uses a hash-suffixed identifier such as
# "mur_agent_runtime-8b77536b4697c3b2", which changes on every rebuild. After
# codesign.sh, the identifier must be exactly the file's basename.
BIN="$WORK/mur-agent-runtime"
cp /usr/bin/true "$BIN"
if [ ! -x "$SCRIPT" ]; then
  fail "stable identifier: $SCRIPT does not exist or is not executable"
else
  env -u MUR_CODESIGN_IDENTITY "$SCRIPT" "$BIN" >/dev/null 2>&1 || true
  got="$(identifier_of "$BIN")"
  if [ "$got" = "mur-agent-runtime" ]; then
    pass "stable identifier = mur-agent-runtime"
  else
    fail "stable identifier: expected 'mur-agent-runtime', got '$got'"
  fi
fi

has_runtime_flag() {
  # Capture first, then grep. Piping codesign straight into `grep -q` under
  # `set -o pipefail` is a trap: grep exits on the first match, codesign dies of
  # SIGPIPE (141), and pipefail turns a successful match into a failure.
  local info
  info="$(codesign -d --verbose=2 "$1" 2>&1)" || true
  grep -Eq '^CodeDirectory .*flags=0x[0-9a-f]+\([^)]*runtime' <<<"$info"
}

# --- test: ad-hoc signing leaves the hardened runtime OFF ---------------------
# Dev builds are ad-hoc. Hardened runtime without the get-task-allow
# entitlement blocks lldb from attaching, so it must not be switched on here.
BIN="$WORK/adhoc/mur-agent-runtime"
mkdir -p "$WORK/adhoc"; cp /usr/bin/true "$BIN"
if [ -x "$SCRIPT" ]; then
  env -u MUR_CODESIGN_IDENTITY "$SCRIPT" "$BIN" >/dev/null 2>&1 || true
  if has_runtime_flag "$BIN"; then
    fail "ad-hoc: hardened runtime flag set; expected it absent"
  else
    pass "ad-hoc: no hardened runtime flag"
  fi
fi

# --- test: a real identity turns the hardened runtime ON ----------------------
# Matches release.yml (--options runtime), so a locally signed binary behaves
# like the notarized one. Needs the throwaway identity from
# scripts/test-signing-identity.sh; skipped (not failed) when it is absent.
REAL_ID="Mur Test (${MUR_TEST_SIGNING_OU:-${MUR_TEST_TEAM_ID:-TESTTEAMID123}})"
if ! security find-identity -v -p codesigning 2>/dev/null | grep -qF "\"$REAL_ID\""; then
  echo "skip: real identity: '$REAL_ID' not in keychain (run: bash scripts/test-signing-identity.sh)"
elif [ -x "$SCRIPT" ]; then
  BIN="$WORK/real/mur-agent-runtime"
  mkdir -p "$WORK/real"; cp /usr/bin/true "$BIN"
  MUR_CODESIGN_IDENTITY="$REAL_ID" "$SCRIPT" "$BIN" >/dev/null 2>&1 || true
  info="$(codesign -d --verbose=2 "$BIN" 2>&1)"
  if ! grep -qF "Authority=$REAL_ID" <<<"$info"; then
    fail "real identity: binary not signed by '$REAL_ID'"
  elif has_runtime_flag "$BIN"; then
    pass "real identity: hardened runtime flag set"
  else
    fail "real identity: expected hardened runtime flag, got: $(grep '^CodeDirectory' <<<"$info")"
  fi
fi

if [ "$FAILS" -ne 0 ]; then
  echo "test-codesign: $FAILS failure(s)"
  exit 1
fi
echo "test-codesign: all passed"
