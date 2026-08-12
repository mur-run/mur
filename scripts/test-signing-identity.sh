#!/bin/bash
# Create a throwaway self-signed code-signing identity for the attestation
# behavioral tests. macOS only; exits 0 with a notice on other platforms.
# Prints the OU (via MUR_TEST_SIGNING_OU in GITHUB_ENV when under CI).
set -euo pipefail
if [ "$(uname)" != "Darwin" ]; then
  echo "test-signing-identity: not macOS, skipping"
  exit 0
fi
OU="${MUR_TEST_TEAM_ID:-TESTTEAMID123}"
CN="Mur Test ($OU)"
KC_DIR="${TMPDIR:-/tmp}/mur-attest-keychain"
rm -rf "$KC_DIR"; mkdir -p "$KC_DIR"
KC="$KC_DIR/test.keychain"
P12="$KC_DIR/test.p12"
PASS="mur"
# A plain self-signed cert (CA:TRUE, no EKU) is refused by codesign: the
# Code Signing EKU, a digitalSignature key usage and CA:FALSE are required.
openssl req -x509 -newkey rsa:2048 -keyout "$KC_DIR/key.pem" -out "$KC_DIR/cert.pem" \
  -days 1 -nodes -subj "/OU=$OU/CN=$CN" \
  -addext "basicConstraints=critical,CA:FALSE" \
  -addext "extendedKeyUsage=codeSigning" \
  -addext "keyUsage=critical,digitalSignature" >/dev/null 2>&1
# OpenSSL 3's default PKCS12 MAC is unreadable by the Security framework
# (import dies with "MAC verification failed"); LibreSSL has no -legacy and
# its default output is already legacy-compatible. Try -legacy, else plain.
if ! openssl pkcs12 -legacy -export -out "$P12" -inkey "$KC_DIR/key.pem" -in "$KC_DIR/cert.pem" \
  -passout "pass:$PASS" >/dev/null 2>&1; then
  openssl pkcs12 -export -out "$P12" -inkey "$KC_DIR/key.pem" -in "$KC_DIR/cert.pem" \
    -passout "pass:$PASS" >/dev/null 2>&1
fi
security create-keychain -p "$PASS" "$KC"
security import "$P12" -k "$KC" -P "$PASS" -T /usr/bin/codesign >/dev/null 2>&1
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$PASS" "$KC" >/dev/null 2>&1
# codesign refuses untrusted identities; trust the throwaway root in the user
# domain. This can raise a GUI authorization dialog and hang headless (known
# CI runner hazard), so bound it with a hard deadline and only advertise the
# OU below if the identity is genuinely usable.
perl -e 'alarm 20; exec @ARGV' security add-trusted-cert -d -r trustRoot -k "$KC" "$KC_DIR/cert.pem" >/dev/null 2>&1 || \
  echo "test-signing-identity: trust settings not installed; behavioral matrix will skip"
security default-keychain -s "$KC"
security unlock-keychain -p "$PASS" "$KC"
if security find-identity -v -p codesigning "$KC" | grep -q "Mur Test ($OU)"; then
  echo "test-signing-identity: identity '$CN' ready in $KC"
  echo "MUR_TEST_SIGNING_OU=$OU" >> "${GITHUB_ENV:-/dev/null}"
else
  echo "test-signing-identity: identity not usable; MUR_TEST_SIGNING_OU not exported"
fi
