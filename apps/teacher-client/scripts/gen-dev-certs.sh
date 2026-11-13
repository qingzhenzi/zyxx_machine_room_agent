#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIR="$ROOT/tools/update-server/certs"
FORCE=0
[[ "${1:-}" == "--force" ]] && FORCE=1

mkdir -p "$DIR"

if [[ -f "$DIR/server.crt" && -f "$DIR/server.key" && "$FORCE" -ne 1 ]]; then
  echo "Certificates already exist in $DIR (use --force to regenerate)."
  exit 0
fi

openssl req -x509 -newkey rsa:2048 -sha256 -days 3650 -nodes \
  -keyout "$DIR/server.key" \
  -out "$DIR/server.crt" \
  -subj "/CN=localhost/O=QEStudio Dev" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" \
  -addext "keyUsage=digitalSignature,keyEncipherment" \
  -addext "extendedKeyUsage=serverAuth"

chmod 600 "$DIR/server.key"
chmod 644 "$DIR/server.crt"

echo "Generated self-signed certificate:"
echo "  cert: $DIR/server.crt"
echo "  key:  $DIR/server.key"
echo
echo "Trust it for local tests (choose one):"
echo "  export SSL_CERT_FILE=$DIR/server.crt"
echo "  sudo cp $DIR/server.crt /usr/local/share/ca-certificates/qestudio-dev.crt && sudo update-ca-certificates"
echo "  macOS: sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain $DIR/server.crt"
echo
echo "Start TLS server:"
echo "  node tools/update-server/server.mjs --tls"
