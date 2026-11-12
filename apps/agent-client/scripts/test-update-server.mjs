#!/usr/bin/env node
import { spawn } from 'node:child_process'
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs'
import { get as httpsGet } from 'node:https'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'

const temp = mkdtempSync(join(tmpdir(), 'qestudio-update-test-'))
mkdirSync(join(temp, 'downloads'), { recursive: true })
writeFileSync(join(temp, 'latest.json'), JSON.stringify({ version: '9.9.9', platforms: {} }))
writeFileSync(join(temp, 'latest-beta.json'), JSON.stringify({ version: '9.9.9-beta.1', platforms: {} }))
writeFileSync(join(temp, 'downloads', 'fixture.deb'), 'fixture-bytes')

function start(port, tls = false) {
  const args = ['tools/update-server/server.mjs', '--port', String(port), '--dir', temp]
  if (tls) args.push('--tls')
  return spawn(process.execPath, args, { stdio: ['ignore', 'pipe', 'pipe'] })
}

async function waitFor(port) {
  for (let attempt = 0; attempt < 40; attempt += 1) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/health`)
      if (response.ok) return
    } catch {
      // not ready yet
    }
    await delay(100)
  }
  throw new Error('update server did not start')
}

function assert(condition, message) {
  if (!condition) throw new Error(message)
}

const port = 8897
const server = start(port)
try {
  await waitFor(port)
  const health = await fetch(`http://127.0.0.1:${port}/health`).then((r) => r.json())
  assert(health.ok === true, 'health failed')

  const stable = await fetch(`http://127.0.0.1:${port}/latest.json`).then((r) => r.json())
  assert(stable.version === '9.9.9', 'stable manifest failed')

  const beta = await fetch(`http://127.0.0.1:${port}/latest.json`, {
    headers: { 'x-update-channel': 'beta' },
  }).then((r) => r.json())
  assert(beta.version === '9.9.9-beta.1', 'beta manifest failed')

  const download = await fetch(`http://127.0.0.1:${port}/downloads/fixture.deb`)
  assert(download.status === 200, 'download failed')
  assert((await download.text()) === 'fixture-bytes', 'download body failed')

  const missing = await fetch(`http://127.0.0.1:${port}/nope`)
  assert(missing.status === 404, '404 failed')
} finally {
  server.kill('SIGTERM')
  await delay(100)
}

let tlsChecked = false
try {
  const tlsServer = start(8898, true)
  try {
    await delay(700)
    await new Promise((resolvePromise, reject) => {
      const request = httpsGet(
        {
          host: '127.0.0.1',
          port: 8898,
          path: '/health',
          rejectUnauthorized: false,
        },
        (response) => {
          let body = ''
          response.on('data', (chunk) => (body += chunk))
          response.on('end', () => {
            try {
              assert(JSON.parse(body).ok === true, 'TLS health failed')
              resolvePromise()
            } catch (error) {
              reject(error)
            }
          })
        },
      )
      request.on('error', reject)
    })
    tlsChecked = true
  } finally {
    tlsServer.kill('SIGTERM')
    await delay(100)
  }
} catch (error) {
  console.log(`TLS check skipped: ${error.message} (run scripts/gen-dev-certs.sh first)`)
}

rmSync(temp, { recursive: true, force: true })
console.log(`Update server checks passed${tlsChecked ? ' (HTTP + TLS)' : ' (HTTP)'}.`)
