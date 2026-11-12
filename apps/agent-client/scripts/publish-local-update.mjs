#!/usr/bin/env node
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { basename, join, resolve } from 'node:path'

function argument(name, fallback) {
  const index = process.argv.indexOf(`--${name}`)
  return index >= 0 ? process.argv[index + 1] : fallback
}

const version = argument('version')
const artifact = argument('artifact')
const channel = argument('channel', 'stable')
const platform = argument('platform', 'linux-x86_64')
const notes = argument('notes', '')
const baseUrl = argument('base-url', 'http://127.0.0.1:8787').replace(/\/$/, '')
const outputDir = resolve(argument('output-dir', 'tools/update-server/public'))
const signatureArg = argument('signature', '')
const urlArg = argument('url', '')

if (!version || !artifact) {
  console.error(
    'Usage: node scripts/publish-local-update.mjs --version 0.1.1 --artifact <bundle> --platform linux-x86_64 [--signature <file.sig>] [--channel stable|beta]',
  )
  process.exit(1)
}
if (!existsSync(artifact)) {
  console.error(`Artifact not found: ${artifact}`)
  process.exit(1)
}

const signatureFile = signatureArg || `${artifact}.sig`
if (!existsSync(signatureFile)) {
  console.error(`Signature not found: ${signatureFile}`)
  console.error('Sign it first:')
  console.error(
    `  pnpm tauri signer sign -f ~/.config/qestudio/updater.key -p "$(cat ~/.config/qestudio/updater.password)" --app-version ${version} ${artifact}`,
  )
  process.exit(1)
}

const downloadsDir = join(outputDir, 'downloads')
mkdirSync(downloadsDir, { recursive: true })
const fileName = basename(artifact)
const targetArtifact = join(downloadsDir, fileName)
if (resolve(artifact) !== targetArtifact) copyFileSync(artifact, targetArtifact)

const manifestFile = join(outputDir, channel === 'beta' ? 'latest-beta.json' : 'latest.json')
const existing = existsSync(manifestFile)
  ? JSON.parse(readFileSync(manifestFile, 'utf8'))
  : { version, notes, pub_date: new Date().toISOString(), platforms: {} }

const manifest = {
  version,
  notes: notes || existing.notes || '',
  pub_date: new Date().toISOString(),
  platforms: {
    ...(existing.platforms ?? {}),
    [platform]: {
      signature: readFileSync(signatureFile, 'utf8').trim(),
      url: urlArg || `${baseUrl}/downloads/${encodeURIComponent(fileName)}`,
    },
  },
}
writeFileSync(manifestFile, `${JSON.stringify(manifest, null, 2)}\n`)

console.log(`Published ${channel} ${version} for ${platform}`)
console.log(`  artifact: ${targetArtifact}`)
console.log(`  manifest: ${manifestFile}`)
console.log(`  URL: ${urlArg || `${baseUrl}/downloads/${fileName}`}`)
