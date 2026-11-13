#!/usr/bin/env bash
# Codesign a local macOS .app with a self-signed certificate.
# Gatekeeper will still reject it unless the certificate is explicitly trusted
# on the test machine; this is for local update testing only.
set -euo pipefail

APP="${1:-}"
if [[ -z "$APP" || ! -d "$APP" ]]; then
  echo "Usage: scripts/macos-selfsigned.sh /path/to/QEStudio.app" >&2
  exit 1
fi

WORK="$(mktemp -d -t qestudio-selfsigned.XXXXXX)"
KEYCHAIN="$WORK/qestudio.keychain-db"
IDENTITY="QEStudio Self Signed"

cleanup() {
  security delete-keychain "$KEYCHAIN" >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT

security create-keychain -p qestudio "$KEYCHAIN"
security unlock-keychain -p qestudio "$KEYCHAIN"
security set-keychain-settings -lut 21600 "$KEYCHAIN"

openssl req -x509 -newkey rsa:2048 -sha256 -days 365 -nodes \
  -keyout "$WORK/key.pem" -out "$WORK/cert.pem" \
  -subj "/CN=$IDENTITY/O=QEStudio Local" \
  -addext "keyUsage=digitalSignature" \
  -addext "extendedKeyUsage=codeSigning"

openssl pkcs12 -export \
  -inkey "$WORK/key.pem" \
  -in "$WORK/cert.pem" \
  -out "$WORK/identity.p12" \
  -passout pass:qestudio

security import "$WORK/identity.p12" -k "$KEYCHAIN" -P qestudio -T /usr/bin/codesign
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k qestudio "$KEYCHAIN" >/dev/null

codesign --force --deep --sign "$IDENTITY" --keychain "$KEYCHAIN" "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

echo "Signed with self-signed identity '$IDENTITY'."
echo "For local Gatekeeper testing, add the certificate to the login keychain and trust it."
