import { readFileSync, writeFileSync } from 'node:fs'

function argument(name, fallback) {
  const index = process.argv.indexOf(`--${name}`)
  return index >= 0 ? process.argv[index + 1] : fallback
}

const version = argument('version')
const url = argument('url')
const signatureFile = argument('signature-file')
const notes = argument('notes', '')
const platform = argument('platform', 'linux-x86_64')
const output = argument('output', 'latest.json')

if (!version || !url || !signatureFile) {
  console.error(
    'Usage: node scripts/make-update-manifest.mjs --version 0.2.0 --url https://... --signature-file app.deb.sig [--notes text] [--platform linux-x86_64]',
  )
  process.exit(1)
}

const signature = readFileSync(signatureFile, 'utf8').trim()
const manifest = {
  version,
  notes,
  pub_date: new Date().toISOString(),
  platforms: {
    [platform]: { signature, url },
  },
}

writeFileSync(output, `${JSON.stringify(manifest, null, 2)}\n`)
console.log(`Wrote ${output} for ${version} (${platform}).`)
