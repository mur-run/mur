#!/bin/bash
# Sign MUR binaries with a stable code identity so macOS privacy (TCC) grants
# survive rebuilds.
#
#   scripts/codesign.sh <binary> [<binary> ...]
#
# Identity: $MUR_CODESIGN_IDENTITY, or ad-hoc ("-") when unset.
# Identifier: the file's basename (e.g. "mur-agent-runtime") instead of cargo's
# hash-suffixed "mur_agent_runtime-<hash>", which changes on every build.
#
# Run as the invoking user, never under sudo: the identity lives in the user's
# login keychain, and root cannot use it (errSecInternalComponent).
# Missing paths are skipped — a partial workspace build lacks some binaries.
set -euo pipefail

IDENTITY="${MUR_CODESIGN_IDENTITY:--}"

# Hardened runtime only with a real identity, matching release.yml. Ad-hoc dev
# builds stay without it: they have no get-task-allow entitlement, so the
# runtime would block debuggers attaching to them.
OPTS=()
if [ "$IDENTITY" != "-" ]; then
  OPTS=(--options runtime)
fi

# If the real identity is missing or unusable, codesign fails before touching
# the file, leaving cargo's hash-suffixed linker signature in place. Fall back
# to ad-hoc with the stable identifier instead, but still exit non-zero so the
# caller (build.sh) reports the failure and warns about the ad-hoc signature.
RC=0
for f in "$@"; do
  [ -f "$f" ] || continue
  id="$(basename "$f")"
  if codesign --force -s "$IDENTITY" ${OPTS[@]+"${OPTS[@]}"} --identifier "$id" "$f"; then
    continue
  fi
  RC=1
  if [ "$IDENTITY" != "-" ]; then
    echo "codesign.sh: '$IDENTITY' failed for $f; falling back to ad-hoc" >&2
    codesign --force -s - --identifier "$id" "$f" || true
  fi
done
exit "$RC"
