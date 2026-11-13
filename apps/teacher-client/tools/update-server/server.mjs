#!/usr/bin/env node
import { createReadStream, existsSync, readFileSync, statSync } from 'node:fs'
import { createServer as createHttpServer } from 'node:http'
import { createServer as createHttpsServer } from 'node:https'
import { extname, resolve, sep } from 'node:path'

function argument(name, fallback) {
  const index = process.argv.indexOf(`--${name}`)
  return index >= 0 ? process.argv[index + 1] : fallback
}

const port = Number(argument('port', process.env.PORT ?? 8787))
const host = argument('host', process.env.HOST ?? '127.0.0.1')
const root = resolve(argument('dir', process.env.UPDATE_DIR ?? 'tools/update-server/public'))
const tls = process.argv.includes('--tls')
const certPath = resolve(argument('cert', 'tools/update-server/certs/server.crt'))
const keyPath = resolve(argument('key', 'tools/update-server/certs/server.key'))

const mime = {
  '.json': 'application/json; charset=utf-8',
  '.txt': 'text/plain; charset=utf-8',
  '.html': 'text/html; charset=utf-8',
  '.deb': 'application/vnd.debian.binary-package',
  '.gz': 'application/gzip',
  '.dmg': 'application/x-apple-diskimage',
  '.sig': 'text/plain; charset=utf-8',
  '.exe': 'application/vnd.microsoft.portable-executable',
  '.msi': 'application/x-msi',
}

function json(res, status, body) {
  const payload = JSON.stringify(body)
  res.writeHead(status, {
    'content-type': 'application/json; charset=utf-8',
    'content-length': Buffer.byteLength(payload),
    'cache-control': 'no-store',
    'access-control-allow-origin': '*',
  })
  res.end(payload)
}

function sendFile(req, res, file) {
  const stat = statSync(file)
  res.writeHead(200, {
    'content-type': mime[extname(file)] ?? 'application/octet-stream',
    'content-length': stat.size,
    'cache-control': 'no-store',
    'access-control-allow-origin': '*',
  })
  if (req.method === 'HEAD') {
    res.end()
    return
  }
  createReadStream(file).pipe(res)
}

function handler(req, res) {
  const url = new URL(req.url ?? '/', `http://${req.headers.host ?? 'localhost'}`)
  if (url.pathname === '/health') {
    json(res, 200, { ok: true, service: 'qestudio-update-server', tls })
    return
  }

  if (url.pathname === '/latest.json') {
    const channel = String(
      req.headers['x-update-channel'] ?? url.searchParams.get('channel') ?? 'stable',
    ).toLowerCase()
    if (channel === 'beta' && existsSync(resolve(root, 'latest-beta.json'))) {
      sendFile(req, res, resolve(root, 'latest-beta.json'))
      return
    }
  }

  const relative = decodeURIComponent(url.pathname).replace(/^\/+/, '') || 'index.html'
  const file = resolve(root, relative)
  if (file !== root && !file.startsWith(`${root}${sep}`)) {
    json(res, 403, { error: 'forbidden' })
    return
  }
  if (!existsSync(file) || !statSync(file).isFile()) {
    json(res, 404, { error: 'not_found', path: url.pathname })
    return
  }

  sendFile(req, res, file)
}

const server = tls
  ? createHttpsServer(
      {
        cert: readFileSync(certPath),
        key: readFileSync(keyPath),
      },
      handler,
    )
  : createHttpServer(handler)

server.listen(port, host, () => {
  const scheme = tls ? 'https' : 'http'
  console.log(`QEStudio update server: ${scheme}://${host}:${port}`)
  console.log(`Serving ${root}`)
  console.log(`stable: ${scheme}://${host}:${port}/latest.json`)
  console.log(`beta:   ${scheme}://${host}:${port}/latest.json (X-Update-Channel: beta)`)
})

process.on('SIGINT', () => server.close(() => process.exit(0)))
process.on('SIGTERM', () => server.close(() => process.exit(0)))
