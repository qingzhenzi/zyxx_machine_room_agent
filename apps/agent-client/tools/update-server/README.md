# QEStudio local update server

A zero-dependency fake update host for local updater testing. It serves a
directory over HTTP/HTTPS and supports stable/beta channel selection via
`X-Update-Channel`.

## Start

```sh
pnpm update:certs              # once, generates self-signed localhost cert
pnpm update:serve              # http://127.0.0.1:8787
pnpm update:serve:tls          # https://localhost:8787
```

Routes:

- `GET /health`
- `GET /latest.json`
- `GET /latest.json` + `X-Update-Channel: beta` -> `latest-beta.json` when present
- `GET /downloads/<artifact>`

## Build and publish a signed Linux artifact

The Tauri updater private key/password stay outside the repository:

```sh
TAURI_SIGNING_PRIVATE_KEY="$(cat ~/.config/qestudio/updater.key)" \
TAURI_SIGNING_PRIVATE_KEY_PASSWORD="$(cat ~/.config/qestudio/updater.password)" \
pnpm tauri build --bundles deb \
  --config '{"bundle":{"createUpdaterArtifacts":true}}'
```

Publish the artifact and generated `.sig`:

```sh
pnpm update:publish \
  --version 0.1.1 \
  --artifact src-tauri/target/release/bundle/deb/QEStudio_0.1.1_amd64.deb \
  --signature src-tauri/target/release/bundle/deb/QEStudio_0.1.1_amd64.deb.sig \
  --platform linux-x86_64 \
  --channel stable \
  --notes "Local update test"
```

For beta:

```sh
pnpm update:publish ... --channel beta
```

## Point the client at the fake server

Build a test client with updater enabled and the local endpoint:

```sh
VITE_UPDATER_ENABLED=true \
pnpm tauri build \
  --config src-tauri/tauri.update-http.conf.json \
  --bundles deb
```

- HTTP test config: `src-tauri/tauri.update-http.conf.json`
- HTTPS test config: `src-tauri/tauri.update-https.conf.json`
- HTTP is local-only and requires `dangerousInsecureTransportProtocol`.
- HTTPS uses the generated self-signed cert. Trust it first, or launch with
  `SSL_CERT_FILE="$PWD/tools/update-server/certs/server.crt"` where the
  updater's TLS stack honors it. macOS: add it to the System keychain and trust.

## Verify

```sh
pnpm test:update-server
```

The test starts a temporary server, checks health, stable/beta manifests,
download bytes, 404 handling, and HTTPS when certs exist.
